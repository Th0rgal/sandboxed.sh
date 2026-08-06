/**
 * Hermes controller sessions (crons especially) end their replies with a
 * machine-readable trailer the projects board uses for routing:
 *
 *   [STATE_SIGNATURE: lean-silicon|phase-c-bridges|7a1c0aab/…|external-ci-wait]
 *
 * The first field is the controller's routing key (project slug). The trailer
 * is control-plane metadata, not prose — transcripts shown to a human must
 * strip it, and it doubles as the best available display name for sessions
 * that never got a title (a cron session otherwise renders as "Session
 * cron_bef…").
 */

// A trailer at the very END of a message, tolerating trailing whitespace.
// `[^\]]` (not `.`) so a signature spanning lines still matches.
const TRAILING_SIGNATURE_RE = /\s*\[STATE_SIGNATURE:[^\]]*\]\s*$/;

const SIGNATURE_KEY_RE = /\[STATE_SIGNATURE:\s*([^|\]]+)/g;

/** Remove trailing `[STATE_SIGNATURE: …]` trailer(s) from a message body.
 * Only trailers are stripped — a signature quoted mid-text stays intact. */
export function stripStateSignature(content: string): string {
  let out = content;
  while (TRAILING_SIGNATURE_RE.test(out)) {
    out = out.replace(TRAILING_SIGNATURE_RE, "");
  }
  return out;
}

/** Routing key (first `|`-separated field) of the LAST state signature in a
 * body, or null when the body carries none. */
export function extractStateSignatureKey(content: string): string | null {
  let key: string | null = null;
  for (const match of content.matchAll(SIGNATURE_KEY_RE)) {
    const candidate = match[1]?.trim();
    if (candidate) key = candidate;
  }
  return key;
}

/** Scan a transcript (oldest→newest) for the most recent routing key. */
export function stateSignatureKeyFromMessages(
  messages: Array<{ role?: string | null; content?: string | null }>,
): string | null {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message.role !== "assistant" || !message.content) continue;
    const key = extractStateSignatureKey(message.content);
    if (key) return key;
  }
  return null;
}
