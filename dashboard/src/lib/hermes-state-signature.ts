/**
 * Hermes controller sessions (crons especially) can end their replies with
 * machine-readable trailers the projects board uses for routing and health:
 *
 *   [STATE_SIGNATURE: lean-silicon|phase-c-bridges|7a1c0aab/…|external-ci-wait]
 *   [CTRL: lean-silicon | mode=active | wait=0 | next=certify]
 *
 * The first field is the controller's routing key (project slug). The trailer
 * is control-plane metadata, not prose — transcripts shown to a human must
 * strip it, and it doubles as the best available display name for sessions
 * that never got a title (a cron session otherwise renders as "Session
 * cron_bef…").
 */

// A control-plane trailer at the very END of a message, tolerating trailing
// whitespace. `[^\]]` (not `.`) allows a trailer spanning lines.
const TRAILING_CONTROL_TRAILER_RE =
  /\s*\[(?:STATE_SIGNATURE|CTRL):[^\]]*\]\s*$/;

const SIGNATURE_KEY_RE = /\[STATE_SIGNATURE:\s*([^|\]]+)/g;

/** Remove trailing Hermes control-plane trailer(s) from a message body.
 * Only trailers are stripped — metadata quoted mid-text stays intact. */
export function stripHermesControlTrailers(content: string): string {
  let out = content;
  while (TRAILING_CONTROL_TRAILER_RE.test(out)) {
    out = out.replace(TRAILING_CONTROL_TRAILER_RE, "");
  }
  return out;
}

/** Backward-compatible name for callers outside the dashboard bundle. */
export const stripStateSignature = stripHermesControlTrailers;

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
