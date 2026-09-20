import type { StreamItem } from "./transcriptModel";

/** Approximate context window for a harness. Grok 4.6 matches Cursor's 256K. */
export function contextWindow(backend?: string | null): number {
  switch (backend) {
    case "grok":
      return 256_000;
    case "claudecode":
      return 200_000;
    case "codex":
      return 192_000;
    default:
      return 128_000;
  }
}

function clip(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n);
}

/** Rough token count from visible conversation (~4 chars/token).
 * Tool payloads are capped so a 150-tool run does not peg the ring at 100%. */
export function estimateTokens(items: StreamItem[]): number {
  let chars = 0;
  for (const item of items) {
    if (item.kind === "user" || item.kind === "text") chars += item.text.length;
    else if (item.kind === "think") chars += Math.min(item.text.length, 8_000);
    else if (item.kind === "tool") {
      const args = item.args == null ? "" : typeof item.args === "string" ? item.args : JSON.stringify(item.args);
      const result = item.result == null ? "" : typeof item.result === "string" ? item.result : JSON.stringify(item.result);
      chars += item.name.length + clip(args, 400).length + clip(result, 400).length;
    } else if (item.kind === "error") chars += item.text.length;
  }
  return Math.max(0, Math.ceil(chars / 4));
}

export function contextPct(used: number, window: number): number {
  if (window <= 0) return 0;
  return Math.min(100, Math.round((used / window) * 100));
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}K`;
  if (n < 1_000_000) return `${Math.round(n / 100) / 10}K`.replace(/\.0K$/, "K");
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}
