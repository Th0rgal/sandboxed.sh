import { describe, expect, it } from "vitest";
import { buildTranscript, withoutFiller, type StreamItem } from "../src/transcriptModel";
import type { StreamEvent } from "../src/stream";

const ev = (type: string, data: Record<string, unknown>, sequence: number): StreamEvent =>
  ({ type, data, sequence } as unknown as StreamEvent);
const shape = (items: StreamItem[]) =>
  items.map((i) => (i.kind === "text" ? `text(${JSON.stringify(i.text)},live=${i.live})` : i.kind));

/**
 * What is actually known about the reported lone "." — the limits of the
 * diagnosis, pinned down so the next person does not have to re-derive them.
 *
 * Runtime evidence from the user's mission (4d715149, opencode, builtin/smart,
 * session ses_f3ba99c0…, workspace host): the stored event log contains no
 * period-only text at all. It has the tool reads of `.paloma/controller.md` and
 * `.paloma/attach.md` at seq 9 and 14, then the final answer as a text_delta at
 * seq 25 and an assistant_message at seq 26. The gaps at seq 13 and 18 are
 * consistent with thinking or text that streamed but was never persisted, and
 * OpenCode's own storage (`/var/lib/opencode`, legacy paths) had no matching
 * session. So the "." was a live-only emission and its producer is unidentified
 * — it is specifically *not* established to be adapter filler.
 *
 * `withoutFiller` therefore only ever considers finalized bubbles. These tests
 * record the consequence honestly: a period that is still live is left alone,
 * which is the right trade (it may be the opening token of the answer) but also
 * means the live window the user screenshotted is not covered.
 */
describe("the lone '.' — what the filter does and does not cover", () => {
  it("leaves a still-live period alone, so the streamed window is unchanged", () => {
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
    // Unchanged: hiding a live bubble would risk deleting the start of an answer.
    expect(shape(withoutFiller(items))).toEqual(shape(items));
  });

  it("grows in place when the period shares the producer's bubble", () => {
    // This is why a live prefix must never be suppressed: the "." *is* the
    // first character of the sentence that follows.
    const items = buildTranscript([
      ev("text_delta", { content: ".", mode: "delta" }, 4),
      ev("tool_call", { tool_call_id: "2", name: "read" }, 5),
      ev("text_delta", { content: "The audit is clean.", mode: "delta" }, 7),
    ]);
    expect(shape(items)).toEqual(['text(".The audit is clean.",live=true)', "tool"]);
  });

  it("replays the real stored log with no stray bubble at all", () => {
    // A reload of mission 4d715149 renders cleanly with or without the filter,
    // because nothing period-only was ever persisted.
    const stored = buildTranscript([
      ev("user_message", { content: "What's the status of Pareto audit?" }, 1),
      ev("tool_call", { tool_call_id: "t9", name: "read", args: { path: ".paloma/controller.md" } }, 9),
      ev("tool_result", { tool_call_id: "t9", result: "ok" }, 10),
      ev("tool_call", { tool_call_id: "t14", name: "read", args: { path: ".paloma/attach.md" } }, 14),
      ev("tool_result", { tool_call_id: "t14", result: "ok" }, 15),
      ev("text_delta", { content: "The Pareto audit is complete.", mode: "delta" }, 25),
      ev("assistant_message", { content: "The Pareto audit is complete." }, 26),
    ]);
    const expected = ["user", "tool", "tool", 'text("The Pareto audit is complete.",live=false)'];
    expect(shape(stored)).toEqual(expected);
    expect(shape(withoutFiller(stored))).toEqual(expected);
  });
});
