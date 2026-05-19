/**
 * Streaming-delta continuation detection.
 *
 * Backends (codex, grok, anthropic) emit incremental text/thinking deltas as
 * the model produces tokens. We consolidate consecutive deltas of the same
 * stream into one chat bubble. The naive rule — strict prefix equality —
 * miscategorises plenty of real-world payloads as "new thoughts" because
 * the backends occasionally re-emit a buffer with trailing whitespace
 * trimmed, with a punctuation character added, or with one extra token
 * appended. Each false-negative gets persisted as a fresh chat item and
 * blows up DOM + memory (the "NoNo newNo new CI…" pattern visible on
 * mission `3a902278`).
 *
 * Rules implemented here (all evaluated in `isStreamContinuation`):
 *   1. If either side is empty, treat as a continuation.
 *   2. Strict prefix in either direction → continuation.
 *   3. Strip trailing whitespace + a fixed punctuation set, retest prefix.
 *   4. Tail-tolerance: if the shorter side equals the longer side up to
 *      `TAIL_TOLERANCE` trailing chars, treat as continuation (handles
 *      "X" → "X." → "X. " drift).
 *
 * Pure + dependency-free so it can be unit-tested cheaply and shipped to
 * the iOS app via the same logic on the Swift side (mirrored there in a
 * follow-up commit).
 */

/** How many trailing characters of the *longer* side we ignore when the
 * shorter side is otherwise a prefix. Tuned to absorb punctuation drift
 * ("X" vs "X." vs "X.."), not real new content. */
const TAIL_TOLERANCE = 6;

const TRAILING_NOISE_RE = /[\s.,!?;:'")\]}…—–-]+$/u;

function stripTrailingNoise(text: string): string {
  return text.replace(TRAILING_NOISE_RE, "");
}

export function isStreamContinuation(a: string, b: string): boolean {
  if (!a || !b) return true;
  if (a === b) return true;

  if (a.startsWith(b) || b.startsWith(a)) return true;

  const aTrim = stripTrailingNoise(a);
  const bTrim = stripTrailingNoise(b);
  if (aTrim && bTrim && (aTrim.startsWith(bTrim) || bTrim.startsWith(aTrim))) {
    return true;
  }

  // Tail-tolerant prefix: the shorter buffer matches the longer buffer up
  // to a small trailing window. Lets `X` and `X. ` consolidate without
  // collapsing genuinely different completions.
  const [shorter, longer] = a.length <= b.length ? [a, b] : [b, a];
  if (longer.length - shorter.length > TAIL_TOLERANCE) return false;
  return longer.slice(0, shorter.length) === shorter;
}
