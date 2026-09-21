import { describe, expect, it } from "vitest";
import { buildTranscript, type StreamItem } from "../src/transcriptModel";
import type { StreamEvent } from "../src/stream";
import { missionPhase, phaseIsQuiet } from "../src/missionLaunch";

const ev = (type: string, data: Record<string, unknown>, sequence: number): StreamEvent =>
  ({ type, data, sequence } as unknown as StreamEvent);
const shape = (items: StreamItem[]) =>
  items.map((i) => (i.kind === "text" ? `text(${JSON.stringify(i.text)},live=${i.live})` : i.kind));

/**
 * The reported OpenCode turn — "Worked 5 tools", a lone ".", "Worked 1 tool",
 * then the real answer — characterised rather than suppressed.
 *
 * Runtime evidence from mission 4d715149 (opencode, builtin/smart, session
 * ses_f3ba99c0…): the stored event log contains *no* period-only text at all.
 * It has the tool reads at seq 9 and 14, then the final answer as a text_delta
 * at seq 25 and an assistant_message at seq 26. The gaps at seq 13 and 18 are
 * consistent with thinking/text that was streamed but never persisted, so the
 * "." the user saw was a live-only emission whose producer is not identified.
 * OpenCode's own storage had no matching session either.
 *
 * These tests therefore pin down what the transcript does with such an
 * emission, so the next person can tell a rendering bug from a producer one.
 * Nothing is filtered: on reload the stored events already render cleanly.
 */
describe("a period streamed between tool batches", () => {
  it("stays live, and so is still shown — a filter keyed on finalized bubbles would not touch it", () => {
    const items = buildTranscript([
      ev("tool_call", { tool_call_id: "1", name: "read" }, 2),
      ev("tool_result", { tool_call_id: "1", result: "ok" }, 3),
      ev("text_delta", { content: ".", mode: "delta", bubble_id: "b1" }, 4),
      ev("tool_call", { tool_call_id: "2", name: "read" }, 5),
      ev("text_delta", { content: "The audit is clean.", mode: "delta", bubble_id: "b2" }, 7),
    ]);
    expect(shape(items)).toEqual([
      "tool",
      'text(".",live=true)',
      "tool",
      'text("The audit is clean.",live=true)',
    ]);
  });

  it("grows in place when it is the same bubble, which is why it must not be hidden early", () => {
    // A "." arriving first in the producer's own bubble is the opening token of
    // the answer, not a separate utterance. Suppressing punctuation while a
    // bubble is live would delete the start of this sentence.
    const items = buildTranscript([
      ev("text_delta", { content: ".", mode: "delta" }, 4),
      ev("tool_call", { tool_call_id: "2", name: "read" }, 5),
      ev("text_delta", { content: "The audit is clean.", mode: "delta" }, 7),
    ]);
    expect(shape(items)).toEqual(['text(".The audit is clean.",live=true)', "tool"]);
  });

  it("renders only the stored answer from the persisted log, with no stray bubble", () => {
    // What a reload of mission 4d715149 replays: tools, then the final answer.
    const items = buildTranscript([
      ev("user_message", { content: "What's the status of Pareto audit?" }, 1),
      ev("tool_call", { tool_call_id: "t9", name: "read", args: { path: ".paloma/controller.md" } }, 9),
      ev("tool_result", { tool_call_id: "t9", result: "ok" }, 10),
      ev("tool_call", { tool_call_id: "t14", name: "read", args: { path: ".paloma/attach.md" } }, 14),
      ev("tool_result", { tool_call_id: "t14", result: "ok" }, 15),
      ev("text_delta", { content: "The Pareto audit is complete.", mode: "delta" }, 25),
      ev("assistant_message", { content: "The Pareto audit is complete." }, 26),
    ]);
    expect(shape(items)).toEqual([
      "user",
      "tool",
      "tool",
      'text("The Pareto audit is complete.",live=false)',
    ]);
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
