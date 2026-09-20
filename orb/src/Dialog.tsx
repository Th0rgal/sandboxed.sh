import { createUniqueId, onCleanup, onMount, type JSX } from "solid-js";

import { trapFocus } from "./focusScope";

export function Dialog(p: {
  title: string;
  wide?: boolean;
  onClose: () => void;
  children: JSX.Element;
  footer: JSX.Element;
}) {
  let root!: HTMLDivElement;
  const titleId = createUniqueId();
  onMount(() => onCleanup(trapFocus(root, p.onClose)));
  return (
    <div class="dlg-back" onMouseDown={p.onClose}>
      <div
        ref={root}
        role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1}
        class={`dlg ${p.wide ? "dlg-wide" : ""}`}
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
      >
        <h3 id={titleId}>{p.title}</h3>
        <div class="dlg-body">{p.children}</div>
        <div class="dlg-foot">{p.footer}</div>
      </div>
    </div>
  );
}

export function Field(p: { label: string; children: JSX.Element }) {
  return (
    <label class="field">
      <span>{p.label}</span>
      {p.children}
    </label>
  );
}
