import { describe, expect, it } from "vitest";
import { buildTranscript, isFillerBubble, withoutFiller, type StreamItem } from "../src/transcriptModel";
import type { StreamEvent } from "../src/stream";
import { missionPhase, phaseIsQuiet } from "../src/missionLaunch";

const text = (t: string, live = false): StreamItem => ({ kind: "text", key: `t${t}${live}`, text: t, live });
const tool = (id: string): StreamItem => ({ kind: "tool", key: `tool:${id}`, callId: id, name: "read", args: null, done: true });
const kinds = (items: StreamItem[]) => withoutFiller(items).map((i) => (i.kind === "text" ? `text:${i.text}` : i.kind));

/**
 * Reproduces the reported OpenCode turn: five tool calls, a bare "." emitted as
 * its own finalized assistant bubble, one more tool call, then the real answer.
 */
const PARETO: StreamItem[] = [
  { kind: "user", key: "u1", text: "What's the status of Pareto audit?" },
  tool("a"), tool("b"), tool("c"), tool("d"), tool("e"),
  text("."),
  tool("f"),
  text("The Pareto audit is complete; two findings remain open."),
];

describe("the standalone '.' between tool batches", () => {
  it("is dropped, and the work either side reads as one stretch", () => {
    expect(kinds(PARETO)).toEqual([
      "user", "tool", "tool", "tool", "tool", "tool", "tool",
      "text:The Pareto audit is complete; two findings remain open.",
    ]);
    // The filler was what split "Worked 5 tools" from "Worked 1 tool".
    expect(withoutFiller(PARETO).filter((i) => i.kind === "tool")).toHaveLength(6);
  });

  it("leaves the raw items untouched — this is presentation only", () => {
    const before = [...PARETO];
    withoutFiller(PARETO);
    expect(PARETO).toEqual(before);
    expect(PARETO.some((i) => i.kind === "text" && i.text === ".")).toBe(true);
  });
});

describe("what must never be dropped", () => {
  it("keeps a bubble that is still streaming, so a '.' can grow into a sentence", () => {
    const streaming = [text(".", true), tool("a")];
    expect(kinds(streaming)).toEqual(["text:.", "tool"]);
    // Even mid-stream with later text present, a live bubble stays.
    expect(isFillerBubble(text(".", true), 0, [text(".", true), text("later")])).toBe(false);
  });

  it("keeps punctuation that is the last thing said, rather than showing nothing", () => {
    expect(kinds([{ kind: "user", key: "u", text: "hi" }, text(".")])).toEqual(["user", "text:."]);
    expect(kinds([tool("a"), text("…")])).toEqual(["tool", "text:…"]);
  });

  it("keeps a final answer that legitimately ends in punctuation", () => {
    const items = [text("."), text("Yes — the audit passed.")];
    expect(kinds(items)).toEqual(["text:Yes — the audit passed."]);
  });

  it("keeps Markdown and code that is punctuation-only", () => {
    for (const body of ["---", "***", "___", "|", "```", ">", "- - -", "()", "[]", "{}", "//", "*"]) {
      expect(kinds([text(body), text("after")])).toEqual([`text:${body}`, "text:after"]);
    }
  });

  it("keeps ordinary text and never touches errors", () => {
    expect(kinds([text("."), { kind: "error", key: "e", text: "boom" }])).toEqual(["text:.", "error"]);
    expect(kinds([text("Done."), text("after")])).toEqual(["text:Done.", "text:after"]);
  });

  it("is harness-agnostic: the same rule applies to any producer", () => {
    // Claude Code style: canonical finalized bubbles around tool work.
    const claude = [tool("x"), text(","), tool("y"), text("Answer.")];
    expect(kinds(claude)).toEqual(["tool", "tool", "text:Answer."]);
  });
});

describe("filler through the real reducer", () => {
  const ev = (type: string, data: Record<string, unknown>, sequence: number): StreamEvent =>
    ({ type, data, sequence } as unknown as StreamEvent);

  it("drops a '.' that the producer finalized, and keeps the answer", () => {
    const items = buildTranscript([
      ev("user_message", { content: "status?" }, 1),
      ev("tool_call", { tool_call_id: "1", name: "read" }, 2),
      ev("tool_result", { tool_call_id: "1", result: "ok" }, 3),
      ev("assistant_message", { content: ".", bubble_id: "b1" }, 4),
      ev("tool_call", { tool_call_id: "2", name: "grep" }, 5),
      ev("tool_result", { tool_call_id: "2", result: "ok" }, 6),
      ev("assistant_message", { content: "All clear.", bubble_id: "b2" }, 7),
    ]);
    expect(items.some((i) => i.kind === "text" && i.text === ".")).toBe(true);
    expect(kinds(items)).toEqual(["user", "tool", "tool", "text:All clear."]);
  });

  it("does not drop a '.' that later grows into the answer in the same bubble", () => {
    const items = buildTranscript([
      ev("text_delta", { content: ".", mode: "delta", bubble_id: "b1" }, 1),
      ev("text_delta", { content: "..so the audit passed", mode: "delta", bubble_id: "b1" }, 2),
    ]);
    expect(kinds(items)).toEqual(["text:...so the audit passed"]);
  });
});

describe("no banner for a healthy running mission", () => {
  const phase = (status: string, activity = false) =>
    missionPhase({ status, history: [] } as never, activity);

  it("stays quiet while starting, running and resuming", () => {
    for (const status of ["active", "running", "starting", "resuming"]) {
      expect(phaseIsQuiet(phase(status)), status).toBe(true);
    }
  });

  it("says nothing over a finished mission that produced an answer", () => {
    expect(phaseIsQuiet(phase("completed", true))).toBe(true);
    // ...but explains a finished mission that produced nothing.
    expect(phaseIsQuiet(phase("completed", false))).toBe(false);
  });

  it("still speaks for anything the user must act on or worry about", () => {
    for (const status of ["pending", "queued", "awaiting_user", "paused", "blocked", "failed", "interrupted", "cancelled"]) {
      expect(phaseIsQuiet(phase(status)), status).toBe(false);
    }
  });

  it("still speaks when a remote job is unconfirmed or has stopped", () => {
    const remote = (job: Record<string, unknown>) =>
      missionPhase({ status: "active", history: [], remote_job: job } as never, false);
    expect(phaseIsQuiet(remote({ phase: "submit_ambiguous" }))).toBe(false);
    expect(phaseIsQuiet(remote({ phase: "unobserved" }))).toBe(false);
    expect(phaseIsQuiet(remote({ phase: "lease_only" }))).toBe(false);
    expect(phaseIsQuiet(remote({ phase: "running", node_state: "queued" }))).toBe(false);
    expect(phaseIsQuiet(remote({ phase: "running", node_state: "failed" }))).toBe(false);
    expect(phaseIsQuiet(remote({ phase: "running", node_state: "running" }))).toBe(true);
  });
});

it("preserves a punctuation answer when meaningful text belongs to another turn", () => {
  const items: StreamItem[] = [
    { kind: "text", key: "a", text: ".", live: false },
    { kind: "user", key: "b", text: "Now explain it" },
    { kind: "text", key: "c", text: "An explanation", live: false },
  ];
  expect(withoutFiller(items)).toEqual(items);
});
