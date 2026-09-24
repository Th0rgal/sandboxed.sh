import { For, Show, createEffect, createSignal, createUniqueId, onCleanup, onMount, splitProps, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { ChevronDown } from "./icons";

/** Retains native form/change semantics, with the same popup on macOS and web. */
export function Select(props: JSX.SelectHTMLAttributes<HTMLSelectElement>) {
  const [local, rest] = splitProps(props, ["children", "class", "style"]);
  const id = createUniqueId();
  let select!: HTMLSelectElement;
  let host!: HTMLSpanElement;
  let menu: HTMLDivElement | undefined;
  const [open, setOpen] = createSignal(false);
  const [options, setOptions] = createSignal<{ value: string; label: string; disabled: boolean }[]>([]);
  const [label, setLabel] = createSignal("");
  const [active, setActive] = createSignal(0);
  const [position, setPosition] = createSignal<JSX.CSSProperties>({});
  const sync = () => {
    setOptions(Array.from(select.options).map(o => ({ value: o.value, label: o.label, disabled: o.disabled })));
    setLabel(select.selectedOptions[0]?.label ?? "");
  };
  const close = () => setOpen(false);
  const show = () => {
    if (select.disabled) return;
    sync();
    const r = host.getBoundingClientRect();
    const height = Math.min(240, options().length * 28 + 8);
    const below = window.innerHeight - r.bottom - 8;
    const width = Math.min(window.innerWidth - 16, Math.max(r.width, 180));
    setPosition({ left: `${Math.max(8, Math.min(r.left, window.innerWidth - width - 8))}px`, width: `${width}px`,
      top: `${below >= height ? r.bottom + 4 : Math.max(8, r.top - height - 4)}px`, "max-height": `${Math.min(240, window.innerHeight - 16)}px` });
    setActive(Math.max(0, select.selectedIndex)); setOpen(true);
  };
  const choose = (index: number) => {
    if (options()[index]?.disabled) return;
    select.selectedIndex = index;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    sync(); close(); select.focus();
  };
  const move = (delta: number) => {
    let next = active();
    for (let count = 0; count < options().length; count++) {
      next = (next + delta + options().length) % options().length;
      if (!options()[next].disabled) { setActive(next); break; }
    }
    queueMicrotask(() => document.getElementById(`${id}-${active()}`)?.scrollIntoView({ block: "nearest" }));
  };
  let search = "", lastKey = 0;
  const keydown = (e: KeyboardEvent) => {
    if (e.key === "Tab") { close(); return; }
    if (e.key === "Escape" && open()) { e.preventDefault(); e.stopPropagation(); close(); return; }
    if (["ArrowDown", "ArrowUp", "Enter", " ", "Home", "End"].includes(e.key)) {
      e.preventDefault(); e.stopPropagation();
      if (!open()) { show(); return; }
      if (e.key === "Enter" || e.key === " ") choose(active());
      else if (e.key === "Home") { setActive(options().length - 1); move(1); }
      else if (e.key === "End") { setActive(0); move(-1); }
      else move(e.key === "ArrowDown" ? 1 : -1);
    } else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault(); if (!open()) show();
      search = (Date.now() - lastKey > 700 ? "" : search) + e.key.toLowerCase(); lastKey = Date.now();
      const index = options().findIndex(o => !o.disabled && o.label.toLowerCase().startsWith(search));
      if (index >= 0) setActive(index);
    }
  };
  const outside = (e: PointerEvent) => { if (!host.contains(e.target as Node) && !menu?.contains(e.target as Node)) close(); };
  const scroll = (e: Event) => { if (!host.contains(e.target as Node) && !menu?.contains(e.target as Node)) close(); };
  onMount(() => {
    sync();
    const observer = new MutationObserver(sync); observer.observe(select, { childList: true, subtree: true, attributes: true, characterData: true });
    select.addEventListener("change", sync);
    document.addEventListener("pointerdown", outside);
    document.addEventListener("scroll", scroll, true); window.addEventListener("resize", close);
    onCleanup(() => { observer.disconnect(); select.removeEventListener("change", sync); document.removeEventListener("pointerdown", outside); document.removeEventListener("scroll", scroll, true); window.removeEventListener("resize", close); });
  });
  createEffect(() => { props.value; props.disabled; queueMicrotask(() => { if (select) { sync(); if (select.disabled) close(); } }); });
  return <span ref={host} class={`orb-select ${local.class ?? ""}`} style={local.style}>
    <select {...rest} ref={select} class="orb-select-native" aria-expanded={open()} aria-controls={id} aria-activedescendant={open() ? `${id}-${active()}` : undefined}
      onPointerDown={e => { if (e.button !== 0) return; e.preventDefault(); select.focus(); open() ? close() : show(); }}
      onKeyDown={keydown} onBlur={close}>{local.children}</select>
    <span class="orb-select-label" aria-hidden="true">{label()}</span><span class="orb-select-arrows" aria-hidden="true"><ChevronDown size={10}/><ChevronDown size={10}/></span>
    <Show when={open()}><Portal><div ref={menu} id={id} class="orb-options" role="listbox" style={position()}>
      <For each={options()}>{(o, i) => <div id={`${id}-${i()}`} class="orb-option" title={o.label} role="option" aria-selected={i() === select.selectedIndex} aria-disabled={o.disabled} data-active={i() === active()}
        onPointerMove={() => !o.disabled && setActive(i())} onPointerDown={e => { e.preventDefault(); e.stopPropagation(); choose(i()); }}>{o.label}</div>}</For>
    </div></Portal></Show>
  </span>;
}
