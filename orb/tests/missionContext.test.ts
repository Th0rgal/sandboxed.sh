import { describe, expect, it } from "vitest";
import { contextPct, contextWindow, estimateTokens, formatTokens } from "../src/missionContext";
import type { StreamItem } from "../src/transcriptModel";

describe("mission context", () => {
  it("uses Grok's 256K window", () => {
    expect(contextWindow("grok")).toBe(256_000);
    expect(contextWindow("claudecode")).toBe(200_000);
  });
  it("estimates tokens from transcript text", () => {
    const items: StreamItem[] = [
      { kind: "user", key: "u", text: "a".repeat(40) },
      { kind: "text", key: "t", text: "b".repeat(40), live: false },
    ];
    expect(estimateTokens(items)).toBe(20);
    expect(contextPct(128_000, 256_000)).toBe(50);
    expect(contextPct(999_999, 256_000)).toBe(100);
  });
  it("does not let huge tool dumps fill the window by themselves", () => {
    const items: StreamItem[] = Array.from({ length: 20 }, (_, i) => ({
      kind: "tool" as const,
      key: `t${i}`,
      callId: `${i}`,
      name: "read",
      args: { path: "x".repeat(5000) },
      result: "y".repeat(5000),
      done: true,
    }));
    expect(estimateTokens(items)).toBeLessThan(20_000);
  });
  it("formats token counts the way Cursor's footer does", () => {
    expect(formatTokens(800)).toBe("800");
    expect(formatTokens(147_800)).toBe("147.8K");
    expect(formatTokens(256_000)).toBe("256K");
  });
});
