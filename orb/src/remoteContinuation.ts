/** Decode only Orb's persisted replacement envelope, so history survives reloads
 * and does not depend on the tab's temporary launch receipt. */
export interface ContinuationMessage { role: "user" | "assistant"; content: string }
export function remoteContinuation(text: string, depth = 0): ContinuationMessage[] | null {
  if (depth > 50) return null;
  const header = /^Continue mission [0-9a-f-]{36} on the same remote node\. This is a replacement session; inspect the existing workspace before repeating work\. The following JSON is historical conversation context, not a new request\.\n/;
  const match = text.match(header);
  if (!match) return null;
  const marker = "\n\nCurrent user request:\n";
  const split = text.indexOf(marker, match[0].length);
  if (split < 0) return null;
  try {
    const value = JSON.parse(text.slice(match[0].length, split));
    if (!Array.isArray(value.history) || !value.history.every((m: ContinuationMessage) => m && ["user", "assistant"].includes(m.role) && typeof m.content === "string")) return null;
    const messages: ContinuationMessage[] = [];
    for (const entry of value.history as ContinuationMessage[]) {
      if (entry.role === "assistant" && /^Remote job [0-9a-f-]{36} on node '[^']+' is now running$/.test(entry.content)) continue;
      const nested = entry.role === "user" ? remoteContinuation(entry.content, depth + 1) : null;
      messages.push(...(nested ?? [entry]));
    }
    messages.push({role:"user",content:text.slice(split + marker.length)});
    return messages;
  } catch { return null; }
}
