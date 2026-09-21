/** Scheduler-owned identity envelope; never interpret ordinary user text. */
export function deferredMessages(content: string, source: unknown, parts?: unknown): Array<{ id: string; content: string }> | null {
  if (source !== "scheduler") return null;
  const match = /\n<!-- sandboxed:messages:v1:([A-Za-z0-9+/=]+) -->$/.exec(content);
  if (!match && parts == null) return null;
  try {
    const value: unknown = parts != null ? { messages: parts } : JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(match![1]), c => c.charCodeAt(0))));
    if (!value || typeof value !== "object" || !("messages" in value) || !Array.isArray(value.messages) || !value.messages.length) return null;
    const result: Array<{ id: string; content: string }> = [];
    const seen = new Set<string>();
    for (const part of value.messages) {
      if (!Array.isArray(part) || part.length !== 2 || typeof part[0] !== "string" || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(part[0]) || typeof part[1] !== "string" || seen.has(part[0])) return null;
      seen.add(part[0]); result.push({ id: part[0], content: part[1] });
    }
    return result;
  } catch { return null; }
}
