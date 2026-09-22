import { render, cleanup } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { DelayedTranscriptSkeleton } from "../src/Skeleton";

afterEach(() => { cleanup(); vi.useRealTimers(); });
it("does not flash placeholders for fast reads, but shows them for slow history", () => {
  vi.useFakeTimers();
  const { container, unmount } = render(() => <DelayedTranscriptSkeleton />);
  vi.advanceTimersByTime(299);
  expect(container.querySelector('.sk-transcript')).toBeNull();
  vi.advanceTimersByTime(1);
  expect(container.querySelector('.sk-transcript')).not.toBeNull();
  unmount();
  expect(vi.getTimerCount()).toBe(0);
});
it("cancels the placeholder timer when history arrives quickly", () => {
  vi.useFakeTimers();
  const { unmount } = render(() => <DelayedTranscriptSkeleton />);
  unmount();
  expect(vi.getTimerCount()).toBe(0);
});
