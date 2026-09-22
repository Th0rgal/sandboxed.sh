import { Show, createMemo, createSignal, createEffect, type JSX } from "solid-js";
import { copyText } from "./clipboard";
import { CloseIcon, CopyIcon, CheckIcon } from "./icons";

export function describeError(raw: string, fallback = "Something went wrong") {
  const disk = raw.match(/\((\d+(?:\.\d+)?) GiB required\), but only (\d+(?:\.\d+)?) GiB is free/i);
  if (disk) return { title: "Not enough disk space", message: `${disk[2]} GiB available · ${disk[1]} GiB required, including the safety reserve. Choose another machine or free up space.` };
  if (/parallel_missions_cap|maximum.*parallel|parallel mission limit/i.test(raw)) return { title: "Mission limit reached", message: "Wait for a mission to finish or adjust the parallel mission limit in settings." };
  if (/REMOTE_JOB_STILL_RUNNING/.test(raw)) return { title: "A turn is still running", message: "Wait for it to finish before sending this follow-up. Your draft is kept." };
  if (/REMOTE_RESUME_REQUIRES_REPLACEMENT/.test(raw)) return { title: "Couldn’t resume this session", message: "Your draft is kept. Try again; if the problem persists, fork the conversation into a new mission." };
  if (/Failed to fetch|NetworkError|Load failed|fetch failed/i.test(raw)) return { title: "Can’t reach the backend", message: "Check your connection and backend settings, then try again." };
  const technical = raw.length > 260 || /(?:^\s*[{[]|\\n|Traceback|Internal error|statvfs:)/.test(raw);
  return { title: fallback, message: technical ? "The request could not be completed. See details for the backend response." : raw };
}

/** Inline, non-modal feedback. Never truncates or discards the original error. */
export function ErrorNotice(p: { error: string; title?: string; onDismiss?: () => void; children?: JSX.Element }) {
  const [copied, setCopied] = createSignal(false);
  const [copyError, setCopyError] = createSignal("");
  createEffect(() => { p.error; setCopied(false); setCopyError(""); });
  const copy = async () => {
    const error = p.error;
    try {
      await copyText(error);
      if (p.error === error) { setCopied(true); setCopyError(""); }
    } catch (e) {
      if (p.error === error) { setCopied(false); setCopyError(e instanceof Error ? e.message : String(e)); }
    }
  };
  const info = createMemo(() => describeError(p.error, p.title));
  return <section class="error-notice" role="alert">
    <svg class="error-notice-icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><circle cx="8" cy="8" r="6" /><path d="M8 4.5v4M8 10.5v1" /></svg>
    <div class="error-notice-body"><strong>{info().title}</strong><p>{info().message}</p>
      <Show when={p.children}><div class="error-notice-actions">{p.children}</div></Show>
      <Show when={info().message !== p.error}><details><summary>Technical details</summary><pre>{p.error}</pre></details></Show>
      <Show when={copyError()}><p class="error-copy-status" role="status">{copyError()}</p></Show>
    </div>
    <button type="button" class="icon-btn error-copy" aria-label={copied() ? "Error copied" : "Copy error"} title={copied() ? "Copied" : "Copy error"} onClick={() => void copy()}><Show when={copied()} fallback={<CopyIcon size={14} />}><CheckIcon size={14} /></Show></button>
    <Show when={p.onDismiss}><button type="button" class="icon-btn" aria-label="Dismiss error" onClick={() => p.onDismiss?.()}><CloseIcon size={14} /></button></Show>
  </section>;
}
