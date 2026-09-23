import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  onCleanup,
} from "solid-js";
import { Portal } from "solid-js/web";
import * as Ic from "./icons";
import { GoalTag } from "./goal";
import { localSessionGit } from "./localAgents";

export interface SessionPreviewData {
  id: string;
  title: string;
  local: boolean;
  destination: string;
  directory?: string | null;
  project?: string | null;
  harness?: string;
  model?: string;
  effort?: string;
  context?: number | null;
}
export function SessionPreview(p: { data: SessionPreviewData; goal?: boolean }) {
  const tipId = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [position, setPosition] = createSignal({ left: 0, top: 0, width: 340 });
  const [git, setGit] = createSignal<{
    repository: string;
    branch?: string | null;
  } | null>(null);
  let button!: HTMLButtonElement;
  let panel: HTMLDivElement | undefined;
  let showTimer: ReturnType<typeof setTimeout> | undefined;
  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  let generation = 0;
  let checkedAt = 0;
  const cancelTimers = () => {
    clearTimeout(showTimer);
    clearTimeout(hideTimer);
  };
  const close = () => {
    cancelTimers();
    setOpen(false);
  };
  const place = () => {
    const rect = button.getBoundingClientRect();
    const width = Math.min(360, window.innerWidth - 24);
    setPosition({
      left: Math.max(12, Math.min(rect.left, window.innerWidth - width - 12)),
      top: rect.bottom + 8,
      width,
    });
  };
  const show = () => {
    cancelTimers();
    place();
    setOpen(true);
    if (!p.data.local || !p.data.directory || Date.now() - checkedAt < 15000)
      return;
    checkedAt = Date.now();
    const epoch = generation;
    void localSessionGit(p.data.directory)
      .then((value) => {
        if (epoch === generation) setGit(value);
      })
      .catch(() => {});
  };
  const hover = () => {
    clearTimeout(hideTimer);
    if (!open()) showTimer = setTimeout(show, 220);
  };
  const leave = () => {
    clearTimeout(showTimer);
    hideTimer = setTimeout(close, 140);
  };
  const identity = createMemo(
    () => `${p.data.id}\0${p.data.directory ?? ""}\0${p.data.local}`,
  );
  createEffect(() => {
    identity();
    generation++;
    checkedAt = 0;
    setGit(null);
    close();
  });
  const escape = (e: KeyboardEvent) => {
    if (open() && e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  };
  const outside = (e: PointerEvent) => {
    if (
      !button.contains(e.target as Node) &&
      !panel?.contains(e.target as Node)
    )
      close();
  };
  window.addEventListener("keydown", escape, true);
  window.addEventListener("pointerdown", outside);
  window.addEventListener("resize", close);
  onCleanup(() => {
    generation++;
    cancelTimers();
    window.removeEventListener("keydown", escape, true);
    window.removeEventListener("pointerdown", outside);
    window.removeEventListener("resize", close);
  });
  return (
    <>
      <button
        ref={button}
        class="session-title"
        aria-label={`Session details: ${p.data.title}`}
        aria-describedby={open() ? tipId : undefined}
        onPointerEnter={hover}
        onPointerLeave={leave}
        onFocus={show}
        onBlur={leave}
        onClick={show}
      >
        <Show when={p.goal}><GoalTag class="small" /></Show>
        <span class="session-title-text">{p.data.title}</span>
        <Show when={p.data.local} fallback={<Ic.CloudIcon />}>
          <Ic.LaptopIcon />
        </Show>
      </button>
      <Show when={open()}>
        <Portal>
          <div
            ref={panel}
            id={tipId}
            role="tooltip"
            class="session-preview"
            style={{
              left: `${position().left}px`,
              top: `${position().top}px`,
              width: `${position().width}px`,
              "max-height": `calc(100vh - ${position().top + 12}px)`,
            }}
            onPointerEnter={() => clearTimeout(hideTimer)}
            onPointerLeave={leave}
          >
            <div class="session-preview-title">{p.data.title}</div>
            <Show when={git()}>
              {(g) => (
                <div class="session-preview-row">
                  <Ic.BranchIcon />
                  <div>
                    {g().repository}
                    <Show when={g().branch}>
                      <span class="session-preview-secondary">
                        {g().branch}
                      </span>
                    </Show>
                  </div>
                </div>
              )}
            </Show>
            <Show when={p.data.directory}>
              <div class="session-preview-row">
                <Ic.FolderIcon />
                <span>{p.data.directory}</span>
              </div>
            </Show>
            <div class="session-preview-row">
              <Show when={p.data.local} fallback={<Ic.CloudIcon />}>
                <Ic.LaptopIcon />
              </Show>
              <span>{p.data.destination}</span>
            </div>
            <Show when={p.data.model || p.data.harness}>
              <div class="session-preview-row">
                <Ic.CubeIcon />
                <div>
                  {p.data.model || "Default model"}
                  {p.data.effort ? ` · ${p.data.effort}` : ""}
                  <Show when={p.data.harness}>
                    <span class="session-preview-secondary">
                      {p.data.harness}
                    </span>
                  </Show>
                </div>
              </div>
            </Show>
            <Show when={p.data.context != null}>
              <div class="session-preview-row">
                <Ic.ContextRing pct={p.data.context!} />
                <span>
                  ≈ {p.data.context}% context
                  <span class="session-preview-secondary">
                    Estimated from the visible conversation
                  </span>
                </span>
              </div>
            </Show>
            <Show when={p.data.project}>
              <div class="session-preview-project">
                Project · {p.data.project}
              </div>
            </Show>
          </div>
        </Portal>
      </Show>
    </>
  );
}
