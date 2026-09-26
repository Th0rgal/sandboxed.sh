export interface ForkContext {
  source_mission_id: string;
  source_title?: string | null;
  messages: { role: "user" | "assistant"; content: string }[];
}
/** Only collapse the complete, structured fork envelope; malformed text stays visible. */
export function forkContext(text: string): ForkContext | null {
  if (!text.startsWith("Continue the work from this conversation in a fresh native session.")) return null;
  const start = text.indexOf("\n<fork_context>\n");
  if (start < 0 || !text.endsWith("\n</fork_context>")) return null;
  try {
    const value = JSON.parse(text.slice(start + "\n<fork_context>\n".length, -"\n</fork_context>".length));
    if (typeof value.source_mission_id !== "string" || !Array.isArray(value.messages) || !value.messages.every((m: { role?: string; content?: string }) => m && ["user", "assistant"].includes(m.role ?? "") && typeof m.content === "string")) return null;
    return value;
  } catch { return null; }
}
