import { Show, createSignal, onCleanup, onMount } from "solid-js";

/** Fast history reads never flash placeholder content. New missions skip this. */
export function DelayedTranscriptSkeleton() {
  const [visible, setVisible] = createSignal(false);
  onMount(() => {
    const timer = setTimeout(() => setVisible(true), 300);
    onCleanup(() => clearTimeout(timer));
  });
  return <Show when={visible()}><TranscriptSkeleton /></Show>;
}

/** Placeholders that reuse live row metrics so the dock does not jump when
 * the real payload arrives. */
export function TranscriptSkeleton() {
  return (
    <div class="sk-transcript" aria-hidden="true">
      <div class="st-work-head sk-bar" style={{ width: "36%" }} />
      <div class="sk-lines">
        <i style={{ width: "92%" }} />
        <i style={{ width: "74%" }} />
        <i style={{ width: "88%" }} />
        <i style={{ width: "41%" }} />
      </div>
      <div class="user sk-user" />
      <div class="sk-lines">
        <i style={{ width: "86%" }} />
        <i style={{ width: "63%" }} />
        <i style={{ width: "79%" }} />
      </div>
    </div>
  );
}

export function FileSkeleton() {
  return (
    <div class="sk-transcript" aria-hidden="true">
      <div class="sk-lines">
        <i style={{ width: "28%", height: "18px" }} />
        <i style={{ width: "96%" }} />
        <i style={{ width: "90%" }} />
        <i style={{ width: "94%" }} />
        <i style={{ width: "62%" }} />
        <i style={{ width: "88%" }} />
        <i style={{ width: "70%" }} />
      </div>
    </div>
  );
}

export function ControllerSkeleton() {
  return (
    <div class="sk-transcript" aria-hidden="true">
      <div class="sk-bar" style={{ width: "48%", height: "28px", margin: "0 0 14px" }} />
      <div class="sk-bar" style={{ width: "100%", height: "56px", margin: "0 0 8px" }} />
      <div class="sk-bar" style={{ width: "100%", height: "56px", margin: "0 0 8px" }} />
      <div class="sk-bar" style={{ width: "100%", height: "56px" }} />
    </div>
  );
}
