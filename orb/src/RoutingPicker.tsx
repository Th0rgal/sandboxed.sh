import { Portal } from "solid-js/web";
import { ChevronDown } from "./icons";
import { For, Show, createMemo, createSignal, createUniqueId, onCleanup, type JSX } from "solid-js";

export function RoutingPicker(p: { label: string; value: string; options: { id: string; name: string; detail?: string }[]; onInput: (value: string) => void }) {
  const id = createUniqueId();
  let host!: HTMLDivElement;
  let list: HTMLDivElement | undefined;
  const [position, setPosition] = createSignal<JSX.CSSProperties>({});
  const show = () => {
    const r = host.getBoundingClientRect();
    const height = Math.min(220, matches().length * (p.options.some(o => o.detail) ? 46 : 28) + 8);
    setPosition({ left: `${Math.max(8, Math.min(r.left, window.innerWidth - r.width - 8))}px`, width: `${r.width}px`, top: `${window.innerHeight-r.bottom > height+8 ? r.bottom+4 : Math.max(8, r.top-height-4)}px` });
    setOpen(true);
  };
  const scroll = (e: Event) => { if (!host.contains(e.target as Node) && !list?.contains(e.target as Node)) setOpen(false); };
  const close = () => setOpen(false);
  document.addEventListener("scroll", scroll, true);
  window.addEventListener("resize", close);
  onCleanup(() => { document.removeEventListener("scroll", scroll, true); window.removeEventListener("resize", close); });
  const [open, setOpen] = createSignal(false);
  const [active, setActive] = createSignal(-1);
  const matches = createMemo(() => p.options.filter(o => `${o.name} ${o.id}`.toLowerCase().includes(p.value.toLowerCase())).slice(0, 30));
  const choose = (value: string) => { p.onInput(value); setOpen(false); setActive(-1); };
  return <div class="routing-picker" ref={host}>
    <input class="s-input" role="combobox" aria-label={p.label} aria-autocomplete="list" aria-expanded={open() && matches().length > 0} aria-controls={id}
      aria-activedescendant={open() && active() >= 0 ? `${id}-${active()}` : undefined} value={p.value}
      onFocus={() => { show(); setActive(-1); }} onBlur={() => setOpen(false)}
      onInput={e => { p.onInput(e.currentTarget.value); show(); setActive(-1); }}
      onKeyDown={e => {
        if (e.key === "Escape" && open()) { e.preventDefault(); e.stopPropagation(); setOpen(false); }
        if (["ArrowDown", "ArrowUp"].includes(e.key) && matches().length) { e.preventDefault(); show(); setActive(i => ((i < 0 && e.key === "ArrowUp" ? 0 : i) + (e.key === "ArrowDown" ? 1 : matches().length - 1) + matches().length) % matches().length); document.getElementById(`${id}-${active()}`)?.scrollIntoView?.({ block: "nearest" }); }
        if (e.key === "Enter" && open() && active() >= 0 && matches()[active()]) { e.preventDefault(); choose(matches()[active()].id); }
      }} />
    <span class="orb-select-arrows" aria-hidden="true"><ChevronDown size={10}/><ChevronDown size={10}/></span>
    <Show when={open() && matches().length > 0}>
      <Portal><div ref={list} style={position()} class="routing-picker-list orb-options" role="listbox" id={id} aria-label={`${p.label} suggestions`}>
        <For each={matches()}>{(o, i) => <div role="option" id={`${id}-${i()}`} aria-selected={active() === i()} class="routing-picker-option"
          onPointerDown={e => { e.preventDefault(); choose(o.id); }}>
          <span title={o.id}>{o.name || o.id}</span><Show when={o.detail}><small>{o.detail}</small></Show>
        </div>}</For>
      </div></Portal>
    </Show>
  </div>;
}
