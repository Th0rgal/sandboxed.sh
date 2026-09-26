import { Match, Switch } from "solid-js";
/** Resource glyphs share the chart palette without relying on color alone. */
export function ResourceIcon(p: { kind: string }) {
 return <svg class={`resource-icon resource-${p.kind.toLowerCase()}`} width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><Switch>
 <Match when={p.kind === "CPU"}><rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 1v4m6-4v4M9 19v4m6-4v4M1 9h4m-4 6h4m14-6h4m-4 6h4"/><rect x="9" y="9" width="6" height="6" rx="1"/></Match>
 <Match when={p.kind === "Memory"}><rect x="3" y="6" width="18" height="11" rx="2"/><path d="M7 10v3m5-3v3m5-3v3M6 17v3m4-3v3m4-3v3m4-3v3"/></Match>
 <Match when={p.kind === "Disk"}><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 14h18m-5 3h2"/><circle cx="7" cy="17" r=".5"/></Match>
 <Match when={p.kind === "GPU"}><rect x="3" y="5" width="18" height="13" rx="2"/><circle cx="11" cy="11.5" r="3.5"/><path d="M7 18v3m4-3v3m4-3v3m3-12v5M1 5v15"/></Match>
 </Switch></svg>;
}
