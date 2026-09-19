import { onCleanup, onMount, type JSX } from "solid-js";

export function Dialog(p: {
  title: string;
  onClose: () => void;
  children: JSX.Element;
  footer: JSX.Element;
}) {
  onMount(() => {
    const k = (e: KeyboardEvent) => {
      if (e.key === "Escape") p.onClose();
    };
    window.addEventListener("keydown", k);
    onCleanup(() => window.removeEventListener("keydown", k));
  });
  return (
    <div class="dlg-back" onMouseDown={p.onClose}>
      <div
        class="dlg"
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
      >
        <h3>{p.title}</h3>
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
