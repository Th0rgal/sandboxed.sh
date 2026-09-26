/** Recover human text from the old remote terminal envelope, without interpreting
 * arbitrary assistant JSON or throwing away the original diagnostic log. */
export function remoteLog(raw: string): { text: string; details?: string } {
  if (!/^Remote node '[^']+' job [0-9a-f-]{36} (?:finished with|reached) state '/.test(raw)) return { text: raw };
  const marker = raw.indexOf("\n\nlog tail:\n");
  if (marker < 0) return { text: raw };
  const log = raw.slice(marker + "\n\nlog tail:\n".length);
  const parts: string[] = [];
  const seen = new Set<string>();
  for (const line of log.split("\n")) {
    try {
      const event = JSON.parse(line);
      if (typeof event?.sessionID !== "string" || !event.sessionID.startsWith("ses_") || event.type !== "text" || typeof event.part?.text !== "string") continue;
      const id = event.part.id;
      if (typeof id === "string" && seen.has(id)) continue;
      if (typeof id === "string") seen.add(id);
      parts.push(event.part.text);
    } catch { /* A log tail can begin in the middle of a JSON line. */ }
  }
  if (!parts.length) {
    // Claude's remote runner already returns plain Markdown. Only unwrap the
    // exact successful receipt; failures and unknown structured logs stay visible.
    const success = /^Remote node '[^']+' job [0-9a-f-]{36} finished with state 'succeeded' \(exit Some\(0\)\)$/.test(raw.slice(0, marker));
    if (success && log.trim() && !/^[\s]*[\[{]/.test(log)) return { text: log, details: raw };
    return { text: raw };
  }
  const failed = !/finished with state 'succeeded'/.test(raw.slice(0, marker));
  return { text: (failed ? raw.slice(0, marker) + "\n\n" : "") + parts.join("\n\n"), details: raw };
}
