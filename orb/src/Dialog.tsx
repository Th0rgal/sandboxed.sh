import { ErrorNotice } from "./ErrorNotice";
import { Show, createContext, createSignal, createUniqueId, onCleanup, onMount, splitProps, useContext, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { trapFocus } from "./focusScope";
import { CloseIcon } from "./icons";
import "./Dialog.css";

type Layer = { root: () => HTMLDivElement; parent?: Layer };
const DialogContext = createContext<Layer>();
const [layers, setLayers] = createSignal<Layer[]>([]);
let savedOverflow = "";

export function Dialog(p: {
  anchor?: HTMLElement;
  title: string;
  description?: string;
  hint?: string;
  size?: "compact" | "wide";
  class?: string;
  busy?: boolean;
  initialFocus?: () => HTMLElement | undefined;
  onClose: () => void;
  children: JSX.Element;
  footer?: JSX.Element;
}) {
  let root!: HTMLDivElement;
  let pressedOutside = false;
  const parent = useContext(DialogContext);
  const layer: Layer = { root: () => root, parent };
  const titleId = createUniqueId();
  const descriptionId = createUniqueId();
  const index = () => layers().indexOf(layer);
  const top = () => layers().at(-1) === layer;
  const close = () => { if (top() && !p.busy) p.onClose(); };
  onMount(() => {
    if (p.anchor) {
      const place = () => {
        const a = p.anchor!.getBoundingClientRect(), r = root.getBoundingClientRect();
        root.style.left = `${Math.max(8, Math.min(a.right + 8, window.innerWidth - r.width - 8))}px`;
        root.style.top = `${Math.max(8, Math.min(a.top, window.innerHeight - r.height - 8))}px`;
      };
      const observer = new ResizeObserver(place);
      observer.observe(root);
      window.addEventListener("resize", place);
      place();
      onCleanup(() => { observer.disconnect(); window.removeEventListener("resize", place); });
    }
    const returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!layers().length) {
      savedOverflow = document.body.style.overflow;
      document.body.style.overflow = "hidden";
    }
    // Solid can mount a portalled child before its parent. Insert the parent below it.
    setLayers(current => {
      const child = current.findIndex(item => item.parent === layer);
      return child < 0 ? [...current, layer] : [...current.slice(0, child), layer, ...current.slice(child)];
    });
    const releaseFocus = trapFocus(root, close, {
      parent: parent?.root,
      returnFocus,
      initialFocus: () => p.initialFocus?.() ?? root.querySelector<HTMLElement>("[autofocus], .dlg-body input:not(:disabled), .dlg-body textarea:not(:disabled)") ?? undefined,
    });
    onCleanup(() => {
      setLayers(current => current.filter(item => item !== layer));
      releaseFocus();
      if (!layers().length) document.body.style.overflow = savedOverflow;
    });
  });
  return <DialogContext.Provider value={layer}><Portal>
    <div class="dlg-back" classList={{ "dlg-back-dim": index() === 0 && !p.anchor }}
      style={{ "z-index": 1300 + Math.max(0, index()) }}
      onPointerDown={e => { pressedOutside = e.target === e.currentTarget && top(); }}
      onPointerUp={e => { const dismiss = pressedOutside && e.target === e.currentTarget; pressedOutside = false; if (dismiss) close(); }}
      onPointerCancel={() => { pressedOutside = false; }}>
      <div ref={root} role="dialog" aria-modal="true" aria-labelledby={titleId}
        aria-describedby={p.description ? descriptionId : undefined} aria-busy={p.busy || undefined}
        inert={index() >= 0 && !top()} tabIndex={-1}
        style={p.anchor ? { position: "fixed", width: "var(--anchored-dialog-width, 380px)", "max-width": "calc(100vw - 16px)" } : undefined}
        class={`dlg ${p.size === "wide" ? "dlg-wide" : ""} ${p.class ?? ""}`}>
        <header class="dlg-head">
          <h3 id={titleId}>{p.title}</h3>
          <Show when={p.hint}><span class="dlg-hint">{p.hint}</span></Show>
          <button type="button" class="dlg-close" aria-label="Close" disabled={p.busy} onClick={close}><CloseIcon size={16} /></button>
        </header>
        <div class="dlg-body">
          <Show when={p.description}><p class="dlg-description" id={descriptionId}>{p.description}</p></Show>
          {p.children}
        </div>
        <Show when={p.footer}><footer class="dlg-foot">{p.footer}</footer></Show>
      </div>
    </div>
  </Portal></DialogContext.Provider>;
}
export function DialogButton(p: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "primary" | "secondary" | "destructive" }) {
  const [local, rest] = splitProps(p, ["variant", "class", "type", "onClick"]);
  return <button {...rest} type={local.type ?? "button"} class={`dlg-button dlg-button-${local.variant ?? "secondary"} ${local.class ?? ""}`} onClick={event => {
    // WebKit does not focus buttons on click. Preserve the opener for nested dialogs.
    event.currentTarget.focus();
    const handler = local.onClick;
    if (typeof handler === "function") handler(event);
    else if (handler) handler[0](handler[1], event);
  }} />;
}

/** Small naming form; the dialog owns all overlay and keyboard behavior. */
export function PromptSheet(p: {
  class?: string;
  anchor?: HTMLElement;
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
  return <Dialog anchor={p.anchor} title={p.title} hint={p.hint} class={`prompt-sheet ${p.class ?? ""}`} busy={p.busy} onClose={p.onClose} footer={p.footer}>
    <form class="prompt-row" onSubmit={e => { e.preventDefault(); if (!p.busy && !p.disabled) p.onAction(); }}>
      <input class="prompt-input" autofocus aria-label={p.label ?? p.title} placeholder={p.placeholder}
        value={p.value} disabled={p.busy} onInput={e => p.onInput(e.currentTarget.value)} />
      <DialogButton type="submit" variant="primary" disabled={p.busy || p.disabled}>{p.busy ? "Working…" : p.action}</DialogButton>
    </form>
    {p.children}
    <Show when={p.error}><ErrorNotice error={p.error!} /></Show>
  </Dialog>;
}

export function ConfirmDialog(p: {
  title: string;
  description: string;
  action: string;
  cancelLabel?: string;
  destructive?: boolean;
  busy?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onClose: () => void;
}) {
  let cancel!: HTMLButtonElement;
  return <Dialog title={p.title} description={p.description} busy={p.busy} onClose={p.onClose} initialFocus={() => cancel}
    footer={<>
      <DialogButton ref={cancel} disabled={p.busy} onClick={p.onClose}>{p.cancelLabel ?? "Cancel"}</DialogButton>
      <DialogButton variant={p.destructive ? "destructive" : "primary"} disabled={p.busy} onClick={() => { if (!p.busy) p.onConfirm(); }}>{p.busy ? "Working…" : p.action}</DialogButton>
    </>}>
    <Show when={p.error}><ErrorNotice error={p.error!} /></Show>
  </Dialog>;
}

export function Field(p: { label: string; children: JSX.Element }) {
  return <label class="field"><span>{p.label}</span>{p.children}</label>;
}
