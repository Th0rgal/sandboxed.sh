import { For, onCleanup, onMount, type JSX } from "solid-js";

export type MenuEntry =
  | { kind: "sep" }
  | { kind: "item"; label: string; icon?: (p: { size?: number }) => JSX.Element; danger?: boolean; openOnHover?: boolean; onClick: (anchor?: HTMLButtonElement) => void };

export function MenuList(p: { items: MenuEntry[]; onPick?: () => void; onDismissSubmenu?: () => void }) {
  let hoverTimer: ReturnType<typeof setTimeout> | undefined;
  const clearHover = () => { clearTimeout(hoverTimer); hoverTimer = undefined; };
  const pick = (it: Extract<MenuEntry, { kind: "item" }>, anchor: HTMLButtonElement) => {
    clearHover();
    if (!it.openOnHover) p.onPick?.();
    it.onClick(anchor);
  };
  onCleanup(clearHover);
  return (
    <For each={p.items}>
      {(it) =>
        it.kind === "sep" ? (
          <div class="menu-sep" />
        ) : (
          <button
            role="menuitem"
            class={`menu-item ${it.danger ? "danger" : ""}`}
            aria-haspopup={it.openOnHover ? "menu" : undefined}
            onMouseEnter={e => { clearHover(); const anchor = e.currentTarget; if (!it.openOnHover) p.onDismissSubmenu?.(); if (it.openOnHover) hoverTimer = setTimeout(() => pick(it, anchor), 180); }}
            onMouseLeave={clearHover}
            onFocus={() => { if (!it.openOnHover) p.onDismissSubmenu?.(); }}
            onKeyDown={e => { if (it.openOnHover && e.key === "ArrowRight") { e.preventDefault(); pick(it, e.currentTarget); } }}
            onClick={e => pick(it, e.currentTarget)}
          >
            <span class="menu-ico">{it.icon && <it.icon />}</span>
            {it.label}
            {it.openOnHover && <span style={{ "margin-left": "auto", "padding-left": "12px" }} aria-hidden="true">›</span>}
          </button>
        )
      }
    </For>
  );
}

export function PopupMenu(p: { x: number; y: number; items: MenuEntry[]; onClose: () => void; focus?: boolean; onDismissSubmenu?: () => void; children?: JSX.Element }) {
  let dismissTimer: ReturnType<typeof setTimeout> | undefined;
  const cancelDismiss = () => clearTimeout(dismissTimer);
  onCleanup(cancelDismiss);
  let el!: HTMLDivElement;
  let trigger: HTMLElement | null = null;
  onMount(() => {
    trigger = document.activeElement as HTMLElement | null;
    const buttons = () => Array.from(el.querySelectorAll<HTMLButtonElement>(":scope > button:not(:disabled)"));
    // Keyboard open focuses the first item; pointer open uses hover only (no focus ring).
    if (p.focus !== false) buttons()[0]?.focus();
    const r = el.getBoundingClientRect();
    const dx = Math.min(0, window.innerWidth - 8 - r.right);
    const dy = Math.min(0, window.innerHeight - 8 - r.bottom);
    if (dx || dy) {
      el.style.left = `${p.x + dx}px`;
      el.style.top = `${p.y + dy}px`;
    }
    const down = (e: PointerEvent) => {
      if (!el.contains(e.target as Node)) p.onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault(); e.stopPropagation(); p.onClose(); trigger?.focus();
      } else if (el.contains(e.target as Node) && (e.target as HTMLElement).closest('[role="menu"]') !== el) {
        return;
      } else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) {
        e.preventDefault();
        const items = buttons();
        const index = items.indexOf(document.activeElement as HTMLButtonElement);
        const next = e.key === "Home" || (e.key === "ArrowDown" && index < 0) ? 0
          : e.key === "End" || (e.key === "ArrowUp" && index < 0) ? items.length - 1
          : (index + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
        items[next]?.focus();
      } else if (e.key === "Tab") p.onClose();
    };
    window.addEventListener("pointerdown", down, true);
    window.addEventListener("keydown", key);
    onCleanup(() => {
      window.removeEventListener("pointerdown", down, true);
      window.removeEventListener("keydown", key);
    });
  });
  return (
    <div
      ref={el}
      role="menu"
      class="menu popup-menu"
      style={{ left: `${p.x}px`, top: `${p.y}px` }}
      onMouseEnter={cancelDismiss}
      onMouseLeave={() => { cancelDismiss(); dismissTimer = setTimeout(() => p.onDismissSubmenu?.(), 180); }}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <MenuList items={p.items} onDismissSubmenu={p.onDismissSubmenu} onPick={() => { p.onClose(); trigger?.focus(); }} />
      {p.children}
    </div>
  );
}
