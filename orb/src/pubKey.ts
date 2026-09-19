export async function readPalomaPub(): Promise<string> {
  try {
    const g = window as unknown as { __TAURI_INTERNALS__?: { invoke?: (c: string) => Promise<string> }; __TAURI__?: { core?: { invoke?: (c: string) => Promise<string> } } };
    const invoke = g.__TAURI__?.core?.invoke ?? g.__TAURI_INTERNALS__?.invoke;
    if (invoke) {
      const v = await invoke("paloma_ssh_pubkey");
      if (v) return v.trim();
    }
  } catch {
    /* fall through */
  }
  try {
    const r = await fetch("/__paloma_pub");
    if (r.ok) return (await r.text()).trim();
  } catch {
    /* ignore */
  }
  return "";
}
