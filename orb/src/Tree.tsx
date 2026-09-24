import { For, Show, createEffect, createMemo, createSignal, type JSX } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { visibleTree, type TreeNode, type TreeRow } from "./treeModel";

export function TreeConnectors(p: { row: TreeRow<unknown> }) {
  return <span class="tree-connectors" aria-hidden="true">
    <For each={p.row.continuations}>{column => <i class="tree-rail" style={{ "--column": column }} />}</For>
    <Show when={p.row.depth > 0}>
      <i class={`tree-junction ${p.row.following ? "continues" : "last"}`} style={{ "--column": p.row.depth - 1 }} />
    </Show>
    <Show when={p.row.connectsChildren}><i class="tree-child-link" style={{ "--column": p.row.depth }} /></Show>
  </span>;
}

export function SidebarTree<T>(p: { nodes: TreeNode<T>[]; label: string; selected: string | null; render: (row: TreeRow<T>) => JSX.Element }) {
  const [pointerFocus, setPointerFocus] = createSignal(false);
  const visible = createMemo(() => visibleTree(p.nodes));
  const [rows, setRows] = createStore<TreeRow<T>[]>([]);
  createEffect(() => setRows(reconcile(visible(), { key: "id" })));
  const keydown: JSX.EventHandler<HTMLDivElement, KeyboardEvent> = e => {
    const target = e.target as HTMLElement;
    const entry = target.closest<HTMLElement>(".tree-entry");
    if (!entry || target !== entry.querySelector("button")) return;
    const all = [...e.currentTarget.querySelectorAll<HTMLElement>(".tree-entry")];
    const index = all.indexOf(entry), row = rows[index];
    // Loading/empty notes have no interactive target. Skip them so an arrow
    // key cannot trap focus before the next actionable row.
    const focusable = all.filter(el => el.querySelector("button"));
    const focusIndex = focusable.indexOf(entry);
    let next: HTMLElement | undefined;
    switch (e.key) {
      case "ArrowDown": next = focusable[focusIndex + 1]; break;
      case "ArrowUp": next = focusable[focusIndex - 1]; break;
      case "Home": next = focusable[0]; break;
      case "End": next = focusable.at(-1); break;
      case "ArrowRight":
        if (row.expanded === false) target.click();
        else if (row.connectsChildren) next = all[index + 1];
        break;
      case "ArrowLeft":
        if (row.expanded) target.click();
        else next = all.find(el => el.dataset.treeId === row.parentId);
        break;
      default: return;
    }
    e.preventDefault();
    next?.querySelector<HTMLButtonElement>("button")?.focus();
  };
  return <div class="sidebar-tree" role="tree" aria-label={p.label} data-pointer-focus={pointerFocus() ? "true" : undefined}
    onPointerDown={() => setPointerFocus(true)}
    onKeyDown={e => { if (["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End", "Tab"].includes(e.key)) setPointerFocus(false); keydown(e); }}
    onClick={e => {
      // WebKit on macOS does not focus buttons on click. Keep keyboard actions
      // (including cut/paste) attached to the row the user just selected.
      const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".tree-entry button");
      if (button && e.currentTarget.contains(button)) button.focus({ preventScroll: true });
    }}>
    <For each={rows}>{row => <div class="tree-entry" data-tree-id={row.id} data-depth={row.depth}
      role="treeitem" aria-level={row.depth + 1} aria-posinset={row.position} aria-setsize={row.size}
      aria-expanded={row.expanded} aria-selected={p.selected === row.id}
      style={{ "--depth": row.depth }}>
      {p.render(row)}<TreeConnectors row={row} />
    </div>}</For>
  </div>;
}
