/**
 * Clipboard writes that fail loudly. The Tauri webview and any non-secure
 * context can leave `navigator.clipboard` undefined, and a user gesture can
 * still be refused by the platform — a silent no-op there looks exactly like a
 * successful copy, so every caller gets a message it can show instead.
 */
export async function copyText(text: string): Promise<void> {
  const clipboard = navigator.clipboard;
  if (!clipboard?.writeText) throw new Error("Clipboard is unavailable in this window.");
  try {
    await clipboard.writeText(text);
  } catch (e) {
    throw new Error(`Copy was refused: ${e instanceof Error ? e.message : String(e)}`);
  }
}
