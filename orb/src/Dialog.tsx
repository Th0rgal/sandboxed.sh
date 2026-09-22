import { ErrorNotice } from "./ErrorNotice";
import { Show, createUniqueId, onCleanup, onMount, type JSX } from "solid-js";

import { trapFocus } from "./focusScope";
import { CloseIcon } from "./icons";

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

/** Cursor-style create/rename sheet: title + hint + X, input and action on one row. */
export function PromptSheet(p: {
  class?: string;
  title: string;
  hint?: string;
  label?: string;
  placeholder?: string;
  value: string;
  onInput: (value: string) => void;
  action: string;
  onAction: () => void;
  onClose: () => void;
  busy?: boolean;
  disabled?: boolean;
  error?: string | null;
  children?: JSX.Element;
  footer?: JSX.Element;
}) {
  let root!: HTMLDivElement;
  const titleId = createUniqueId();
  onMount(() => onCleanup(trapFocus(root, () => { if (!p.busy) p.onClose(); })));
  return (
    <div class="dlg-back" onMouseDown={() => { if (!p.busy) p.onClose(); }}>
      <div
        ref={root}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        class={`dlg prompt-sheet ${p.class ?? ""}`}
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="prompt-head">
          <h3 id={titleId}>{p.title}</h3>
          <Show when={p.hint}><span class="prompt-hint">{p.hint}</span></Show>
          <button type="button" class="prompt-x" tabIndex={-1} aria-label="Close" disabled={p.busy} onClick={p.onClose}>
            <CloseIcon size={14} />
          </button>
        </div>
        <form class="prompt-row" onSubmit={(e) => { e.preventDefault(); if (!p.busy && !p.disabled) p.onAction(); }}>
          <input
            class="prompt-input"
            autofocus
            aria-label={p.label ?? p.title}
            placeholder={p.placeholder}
            value={p.value}
            disabled={p.busy}
            onInput={(e) => p.onInput(e.currentTarget.value)}
          />
          <button type="submit" class="prompt-go" disabled={p.busy || p.disabled}>
            {p.busy ? "…" : p.action}
          </button>
        </form>
        {p.children}
        <Show when={p.error}><ErrorNotice error={p.error!} /></Show>
        <Show when={p.footer}><div class="prompt-foot">{p.footer}</div></Show>
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
