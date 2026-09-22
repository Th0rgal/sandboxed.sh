import { afterEach, expect, it, vi } from "vitest";
import { bufferedOutput } from "../src/localStream";
import { incrementalMarkdown, parseMarkdown } from "../src/Markdown";
afterEach(() => vi.useRealTimers());
it("batches fragments and flushes the authoritative final state immediately", () => {
  vi.useFakeTimers();
  const paint = vi.fn(),
    finish = vi.fn();
  const stream = bufferedOutput<{ text: string }>(paint, finish);
  stream.receive({ text: "Hello", reset: true });
  stream.receive({ text: " world", reset: false });
  expect(paint).not.toHaveBeenCalled();
  vi.advanceTimersByTime(16);
  expect(paint).toHaveBeenCalledExactlyOnceWith("Hello world");
  stream.receive({ text: "!", reset: false });
  stream.receive({ text: "", reset: false, state: { text: "Hello world!" } });
  expect(paint).toHaveBeenLastCalledWith("Hello world!");
  expect(finish).toHaveBeenCalledOnce();
  vi.runAllTimers();
  expect(paint).toHaveBeenCalledTimes(2);
});
it("replaces corrected snapshots and discards scheduled work on disposal", () => {
  vi.useFakeTimers();
  const paint = vi.fn();
  const stream = bufferedOutput(paint, vi.fn());
  stream.receive({ text: "old", reset: true });
  stream.receive({ text: "corrected", reset: true });
  vi.advanceTimersByTime(16);
  expect(paint).toHaveBeenCalledExactlyOnceWith("corrected");
  stream.receive({ text: " discarded", reset: false });
  stream.dispose();
  vi.runAllTimers();
  expect(paint).toHaveBeenCalledOnce();
});
it("preserves completed Markdown blocks and matches full parsing for every prefix", () => {
  const parse = incrementalMarkdown();
  const text =
    "# Title\n\nParagraph **bold**.\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n\n| A | B |\n|---|---|\n| x | y |\n\n- one\n- two\n";
  for (let i = 0; i <= text.length; i++)
    expect(parse(text.slice(0, i))).toEqual(parseMarkdown(text.slice(0, i)));
  const before = parse(text);
  const after = parse(text + "\nLast paragraph");
  expect(after[0]).toBe(before[0]);
  expect(parse("Revised answer")).toEqual(parseMarkdown("Revised answer"));
});

it("follows native channel output without repeatedly requesting snapshots", async () => {
  vi.useFakeTimers();
  const { followLocal, localLiveText } = await import("../src/localAgents");
  let channel: { onmessage: (event: unknown) => void } | undefined;
  const invoke = vi.fn(
    async (_command: string, args: Record<string, unknown>) => {
      channel = args.onEvent as typeof channel;
    },
  );
  vi.stubGlobal("__TAURI__", {
    core: {
      invoke,
      Channel: class {
        onmessage = (_event: unknown) => {};
      },
    },
  });
  try {
    const paint = vi.fn();
    const following = followLocal("stream-test", paint);
    channel!.onmessage({ text: "Start", reset: true });
    channel!.onmessage({ text: " now", reset: false });
    await vi.advanceTimersByTimeAsync(16);
    expect(paint).toHaveBeenCalledExactlyOnceWith("Start now");
    const state = { text: "Start now.", done: true, resumed: false };
    channel!.onmessage({ text: "", reset: false, state });
    expect(await following).toEqual(state);
    expect(localLiveText("stream-test")).toBe("Start now.");
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][0]).toBe("local_agents_subscribe");
  } finally {
    vi.unstubAllGlobals();
  }
});

it("recognizes the actual Tauri permission error for compatibility fallback",async()=>{
 const {missingStreamCommand}=await import('../src/localAgents');
 expect(missingStreamCommand('local_agents_subscribe not allowed. Command not found')).toBe(true);
 expect(missingStreamCommand('no local run')).toBe(false);
});
