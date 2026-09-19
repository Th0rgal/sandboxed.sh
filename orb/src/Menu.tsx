import { For, onCleanup, onMount, type JSX } from "solid-js";

export type MenuEntry =
  | { kind: "sep" }
  | { kind: "item"; label: string; icon?: (p: { size?: number }) => JSX.Element; danger?: boolean; onClick: () => void };

export function MenuList(p: { items: MenuEntry[]; onPick?: () => void }) {
  return (
    <For each={p.items}>
      {(it) =>
        it.kind === "sep" ? (
          <div class="menu-sep" />
        ) : (
          <button
            class={`menu-item ${it.danger ? "danger" : ""}`}
            onClick={() => {
              it.onClick();
              p.onPick?.();
            }}
          >
            <span class="menu-ico">{it.icon && <it.icon />}</span>
            {it.label}
          </button>
        )
      }
    </For>
  );
}

export function PopupMenu(p: { x: number; y: number; items: MenuEntry[]; onClose: () => void }) {
  let el!: HTMLDivElement;
  onMount(() => {
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
      if (e.key === "Escape") p.onClose();
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
      class="menu popup-menu"
      style={{ left: `${p.x}px`, top: `${p.y}px` }}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <MenuList items={p.items} onPick={p.onClose} />
    </div>
  );
}
