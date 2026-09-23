import { hasNativePicker, pickNativeFiles, transferFile, prepareUploads, uploadToken, type UploadedFile, type UploadSource } from "./uploads";
import { readComposerDraft, saveComposerDraft } from "./composerDrafts";
import { readImage, imagePrompt, stageLocalImages, stageRemoteImages, IMAGE_COUNT, type DraftImage } from "./imageAttachments";
import { FilePanelProvider, FilePanelButton } from "./FilePanel";
import { ErrorNotice } from "./ErrorNotice";
import { MissionFailure, LaunchStatus, MissionPending, missionPhase, phaseIsQuiet, rememberLaunch, recalledLaunch, missionDestination, withInitialPrompt, launchError, launchRefusal, nodeLabel, remoteLaunchPreflight, remoteHarnessSupport, remoteLaunchUnconfirmed, missionGoal, missionSettingsIdle, dockModelLabel, type LaunchReceipt, type LaunchRefusal, type RemoteSupport } from "./missionLaunch";
import { goalDraft, goalObjective, goalPrompt, missionTitle, displayTitle, GoalTag, EMPTY_GOAL_ERROR, absorbGoalPrefix, composerModes, filterSlash, slashQuery, modePrompt, ModeChip, type ComposerMode } from "./goal";
import { atQuery, chipToAttachment, filterAttach, insertMention, loadAttachItems, mentionedChips, type AttachChip, type AttachItem } from "./attach";
import { ProjectPicker, ProjectCreation } from "./ProjectPicker";
import { hasFocusScope } from "./focusScope";
import { For, Show, Switch, Match, createMemo, createSignal, createEffect, on, onCleanup, onMount, batch } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { JSX } from "solid-js";
import { projects as seed, LOREM_REPLY, type Agent, type Block, type Turn } from "./data";
import * as Ic from "./icons";
import { ForkMission } from "./ForkMission";
import { Settings } from "./Settings";
import { SessionPreview, type SessionPreviewData } from "./SessionPreview";
import { RoutingSettings, confirmLeaveRouting } from "./RoutingSettings";
import { MACHINES, Machines } from "./Machines";
import { Providers } from "./Providers";
import { PromptSheet } from "./Dialog";
import { MenuList, PopupMenu, type MenuEntry } from "./Menu";
import { MdSource, MdView, mdSource, safeHref, setMdSource, toggleMdSource } from "./Markdown";
import { streamMission, heldAfterHistory, type StreamEvent } from "./stream";
import { latestChecklist } from "./workModel";
import { Transcript, UserTurn, applyStreamEvent, type StreamItem } from "./Transcript";
import { cacheRemember, cacheRecents } from "./pageCache";
import { DEFAULT_EFFORT_LABEL, effortLabel, harnessSupportsEffort, normalizeEffort, supportedEfforts } from "./effort";
import { loadTranscript, peekReadyTranscript, peekTranscriptHeight, prefetchTranscript, putTranscript, putTranscriptHeight, putTranscriptItems } from "./missionCache";
import { DelayedTranscriptSkeleton } from "./Skeleton";
import { visibleTranscript } from "./transcriptModel";
import { mergeById, pollWhileVisible } from "./poll";
import { LiveProjectsSection, ProjectFileView } from "./ProjectFiles";
import { ProjectSettings } from "./ProjectSettings";
import { ExecutionSettings } from "./ExecutionSettings";
import { VoiceButton, ensureVoiceProbe, voiceAvailable } from "./VoiceButton";
import { insertAtCaret } from "./voice";
import { contextPct, contextWindow, estimateTokens, formatTokens } from "./missionContext";
import {
  bindWorkspace,
  followLocal,
  installedIds,
  localBinding,
  localLiveText,
  localFailure,
  recordLocalFailure,
  localRunActive,
  reconcileLocalRun,
  localWorkspace,
  materializeMentions,
  refreshLocalAgents,
  rememberBinding,
  startLocal,
  stopLocal,
  writeLocalFiles,
} from "./localAgents";
import { ControllerView } from "./Controller";
import {
  createMission,
  getMission,
  getRemoteNodes,
  isConnected,
  listMissions,
  listProjects,
  listHarnessChoices,
  shortModelLabel,
  createProject,
  bumpProjects,
  projectsVersion,
  type HarnessChoice,
  cancelMission,
  appendClientTranscript,
  setClientMissionStatus,
  sendMissionMessage,
  ApiError,
  MessageRejectedError,
  connectionVersion,
  getApiUrl,
  updateMissionSettings,
  type Mission,
  type ProjectSummary,
  type RemoteNodeView,
  type RemoteLaunchCapability,
  type RemoteNodesResponse,
  openExternalUrl,
} from "./api";

const PAGES = new Set(["settings", "routing", "machines", "providers", "execution"]);

const MODELS = ["Orb Lorem 4.6 High Fast", "Ipsum 5 Max", "Dolor 4.5 Sonnet", "Auto"];

/** Harness + model chosen for new agents; persisted per user. `effort` is
 * absent for "let the backend decide", and is dropped whenever the selected
 * harness does not accept that level (see `effort.ts`). */
export type HarnessPick = { backend: string; model: string; effort?: string };
const PICK_KEY = "orb.harnessPick";
const loadPick = (): HarnessPick | null => {
  try {
    const raw = localStorage.getItem(PICK_KEY);
    return raw ? (JSON.parse(raw) as HarnessPick) : null;
  } catch {
    return null;
  }
};
const [harnessChoices, setHarnessChoices] = createSignal<HarnessChoice[]>([]);
const [harnessPick, setHarnessPickRaw] = createSignal<HarnessPick | null>(loadPick());
const setHarnessPick = (p: HarnessPick) => {
  setHarnessPickRaw(p);
  try {
    localStorage.setItem(PICK_KEY, JSON.stringify(p));
  } catch {
    /* ignore */
  }
};
/** Preserve an explicit selection; launch validation reports unavailable models.
 * A stored effort is normalized against the stored harness on every read, so a
 * level that harness never accepted can't survive into a create payload. */
const effectivePick = (): HarnessPick | null => {
  const choices = harnessChoices();
  if (!choices.length) return null;
  const stored = harnessPick();
  if (stored && stored.backend !== "gemini") {
    const effort = normalizeEffort(stored.effort, stored.backend);
    return effort ? { ...stored, effort } : { backend: stored.backend, model: stored.model };
  }
  const first = choices.find((c) => c.backend.id === "claudecode") ?? choices[0];
  return { backend: first.backend.id, model: first.models[0].value };
};
const pickLabel = (pick: HarnessPick | null): string => {
  if (!pick) return "Choose model";
  const c = harnessChoices().find((x) => x.backend.id === pick.backend);
  const m = c?.models.find((x) => x.value === pick.model);
  return `${c?.backend.name ?? pick.backend} · ${m ? shortModelLabel(m.label) : pick.model}`;
};
async function refreshHarnessChoices() {
  try {
    setHarnessChoices(await listHarnessChoices());
  } catch {
    /* keep last */
  }
}

/** Inline markup: `code`, **bold**, [label](href). Nesting is limited to code inside bold/link. */
function inline(text: string): JSX.Element[] {
  const out: JSX.Element[] = [];
  const re = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)]+)\)/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1] !== undefined) out.push(<strong>{inline(m[1])}</strong>);
    else if (m[2] !== undefined) out.push(<code>{m[2]}</code>);
    else
      out.push(
        <a
          href={safeHref(m[4]) ?? "#"}
          onClick={(e) => {
            e.preventDefault();
            const href = safeHref(m[4]);
            if (href) void openExternalUrl(href);
          }}
        >
          {inline(m[3])}
        </a>,
      );
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function BlockView(p: { block: Block }) {
  const b = p.block;
  switch (b.kind) {
    case "p":
      return <p>{inline(b.text)}</p>;
    case "ul":
      return <ul>{b.items.map((t) => <li>{inline(t)}</li>)}</ul>;
    case "note":
      return (
        <p class="note">
          {b.strong} <span>{b.rest}</span>
        </p>
      );
    case "tool":
      return (
        <div class="tool">
          <span class="tool-verb">{b.verb}</span>
          <span class="tool-target">{b.target}</span>
          <Show when={b.meta}>
            <span class="tool-meta">{b.meta}</span>
          </Show>
        </div>
      );
    case "code": {
      const [copied, setCopied] = createSignal(false);
      return (
        <div class="codeblock">
          <div class="codeblock-head">
            <span>{b.file ?? b.lang}</span>
            <button
              class="icon-btn sm"
              title="Copy"
              onClick={() => {
                navigator.clipboard?.writeText(b.text);
                setCopied(true);
                setTimeout(() => setCopied(false), 1200);
              }}
            >
              <Show when={copied()} fallback={<Ic.CopyIcon size={14} />}>
                <span class="copied">Copied</span>
              </Show>
            </button>
          </div>
          <pre>{b.text}</pre>
        </div>
      );
    }
  }
}

function AgentTurn(p: { turn: Extract<Turn, { role: "agent" }>; streaming?: boolean }) {
  const [open, setOpen] = createSignal(true);
  const tools = () => p.turn.blocks.filter((b) => b.kind === "tool");
  const rest = () => p.turn.blocks.filter((b) => b.kind !== "tool");
  return (
    <div class="agent-turn">
      <button class="worked" onClick={() => setOpen(!open())} disabled={!tools().length}>
        <Show when={p.streaming} fallback={<>Worked for {p.turn.worked}</>}>
          <span class="shimmer">Working…</span>
        </Show>
        <Show when={tools().length}>
          <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        </Show>
      </button>
      <Show when={open() && tools().length}>
        <div class="tools">
          <For each={tools()}>{(b) => <BlockView block={b} />}</For>
        </div>
      </Show>
      <For each={rest()}>{(b) => <BlockView block={b} />}</For>
    </div>
  );
}

function StatusGlyph(p: { agent: { status: Agent["status"] }; busy: boolean }) {
  return (
    <Switch fallback={<span class="dot" />}>
      <Match when={p.busy || p.agent.status === "running"}>
        <Ic.RunningDots />
      </Match>
      <Match when={p.agent.status === "pr-closed"}>
        <Ic.PrClosedIcon class="c-red" />
      </Match>
      <Match when={p.agent.status === "pr-merged"}>
        <Ic.PrMergedIcon class="c-purple" />
      </Match>
    </Switch>
  );
}

export function Composer(p: {
  revision?: { text: string };
  placeholder: string;
  busy: boolean;
  onSend: (t: string, images: DraftImage[]) => void | boolean | Promise<void | boolean>;
  onStop: () => void;
  autofocus?: boolean;
  tall?: boolean;
  files?: { id: string; name: string }[];
  attached?: string[];
  onToggleFile?: (id: string) => void;
  /** Show the harness + model picker (new agents only). */
  picker?: boolean;
  /** Conversation identity; dictation results for another scope are dropped. */
  scope?: string;
  /** Server-confirmed remote support for a harness on the selected machine (new agents only). */
  remoteSupport?: (backend: string) => { state: RemoteSupport; note: string };
  /** When set, the harness menu only lists these ids (This computer). */
  harnessIds?: string[];
  /** Follow-up: the mission's harness. New agent uses the picker. */
  backend?: string;
  onDraft?: (text: string) => void;
  projectSlug?: string;
  /** Reports what the draft currently mentions; the draft text is the source. */
  onAttachments?: (next: AttachChip[]) => void;
  uploadTarget?: string;
}) {
  const [text, setText] = createSignal("");
  const [uploading, setUploading] = createSignal(false);
  const [uploadError, setUploadError] = createSignal<string | null>(null);
  let uploaded: UploadedFile[] = [];
  let disposed = false;
  onCleanup(() => { disposed = true; });
  let fileInput!: HTMLInputElement;
  const uploadTarget = () => p.uploadTarget ?? "core";
  const attachSources = async (sources: UploadSource[]) => {
    const scope = p.scope;
    const destination = uploadTarget();
    setUploading(true); setUploadError(null); setCtx(false);
    try {
      for (const source of sources) {
        if (disposed || scope !== p.scope) return;
        const file = await transferFile(source, destination);
        if (disposed || scope !== p.scope) return;
        uploaded.push(file);
        const current = ta.selectionStart ?? text().length;
        const token = uploadToken(file.path) + " ";
        const next = insertAtCaret(text(), current, ta.selectionEnd ?? current, token);
        write(next.value); setCaret(next.caret); ta.setSelectionRange(next.caret, next.caret);
      }
    } catch (error) { setUploadError(error instanceof Error ? error.message : String(error)); }
    finally { setUploading(false); }
  };
  const chooseFiles = async () => {
    setCtx(false); setUploadError(null);
    if (!hasNativePicker()) { fileInput.click(); return; }
    const scope = p.scope;
    const selection = uploadTarget();
    try { const files = await pickNativeFiles(); if (!disposed && scope === p.scope && selection === uploadTarget()) await attachSources(files); }
    catch (error) { setUploadError(error instanceof Error ? error.message : String(error)); }
  };
  const [images, setImages] = createSignal<DraftImage[]>([]);
  const [sending, setSending] = createSignal(false);
  const [pendingSend, setPendingSend] = createSignal<{text:string; images:DraftImage[]} | null>(null);
  const [draftReady, setDraftReady] = createSignal(false);
  createEffect(on(() => p.scope, (scope, previous) => {
    if (previous !== undefined && previous !== scope) { setText(""); setImages([]); uploaded = []; }
    setDraftReady(false);
    if (!scope) { setDraftReady(true); return; }
    let current=true;
    onCleanup(() => { current=false; });
    void readComposerDraft(scope).then(draft => {
      if (current && draft && !text() && !images().length) {
        uploaded = (draft.uploads ?? []).map(file => ({...file, connection: file.endpoint === getApiUrl() ? connectionVersion() : -1}));
        setText(draft.text);setImages(draft.images);
        queueMicrotask(() => { if (ta?.isConnected) { ta.value=draft.text; resize(); } });
      }
    }).catch(() => {}).finally(() => { if (current) setDraftReady(true); });
  }));
  createEffect(() => {
    const scope=p.scope;
    if (draftReady() && scope) void saveComposerDraft(scope,{text:pendingSend()?.text ?? text(),images:pendingSend()?.images ?? images(),uploads:uploaded.map(file => ({...file, source:{name:file.source.name,localPath:file.source.localPath}}))}).catch(() => {});
  });
  const [imageError, setImageError] = createSignal<string | null>(null);
  const [readingImages, setReadingImages] = createSignal(false);
  const pasteImages = async (event: ClipboardEvent) => {
    const files = Array.from(event.clipboardData?.files ?? []).filter(file => file.type.startsWith("image/"));
    if (!files.length) return;
    event.preventDefault();
    if (readingImages() || sending()) return;
    setImageError(null);
    if (images().length + files.length > IMAGE_COUNT) { setImageError(`Attach up to ${IMAGE_COUNT} images at a time.`); return; }
    const scope = p.scope;
    setReadingImages(true);
    try { const next = await Promise.all(files.map(readImage)); if (scope === p.scope) setImages(previous => [...previous, ...next]); }
    catch (e) { setImageError(e instanceof Error ? e.message : String(e)); }
    finally { setReadingImages(false); }
  };
  createEffect(on(() => p.scope, () => { setImages([]); setImageError(null); }, {defer:true}));
  // Local voice input (macOS): dictated text lands at the caret, never sends.
  const [voiceActive, setVoiceActive] = createSignal(false);
  ensureVoiceProbe();
  const [mode, setMode] = createSignal<ComposerMode | null>(null);
  const [slashHi, setSlashHi] = createSignal(0);
  const [model, setModel] = createSignal(MODELS[0]);
  const live = () => isConnected() && harnessChoices().length > 0;
  const [menu, setMenu] = createSignal(false);
  const [ctx, setCtx] = createSignal(false);
  const [which, setWhich] = createSignal<"harness" | "model" | "effort" | null>(null);
  const [slashOff, setSlashOff] = createSignal(false);
  const [atOff, setAtOff] = createSignal(false);
  const [atHi, setAtHi] = createSignal(0);
  const [atItems, setAtItems] = createSignal<AttachItem[]>([]);
  const [caret, setCaret] = createSignal(0);
  let ta!: HTMLTextAreaElement;
  const pick = () => effectivePick();
  const backend = () => p.backend ?? pick()?.backend ?? null;
  const modes = createMemo(() => composerModes(backend()));
  const slash = createMemo(() => {
    if (mode() || voiceActive() || slashOff()) return null;
    const q = slashQuery(text());
    if (!q.open) return null;
    const items = filterSlash(modes(), q.query);
    return items.length ? { query: q.query, items } : null;
  });
  const at = createMemo(() => {
    if (voiceActive() || atOff() || slash()) return null;
    const q = atQuery(text(), caret());
    if (!q.open) return null;
    const demo = (p.files ?? []).map((f) => ({
      id: f.id,
      kind: "file" as const,
      section: "Files" as const,
      path: f.name,
      label: f.name,
    }));
    const source = isConnected() ? atItems() : demo;
    const items = filterAttach(source, q.query);
    return { query: q.query, items };
  });
  createEffect(() => {
    slash();
    setSlashHi(0);
  });
  createEffect(() => {
    at();
    setAtHi(0);
  });
  let attachmentLoad: Promise<void> = Promise.resolve();
  createEffect(() => {
    const slug = p.projectSlug;
    if (!slug || !isConnected()) {
      setAtItems([]);
      return;
    }
    let current = true;
    setAtItems([]);
    onCleanup(() => { current = false; });
    attachmentLoad = loadAttachItems(slug).then(items => { if (current) setAtItems(items); })
      .catch(() => { if (current) setAtItems([]); });
  });
  /** What the current draft refers to, resolved against this project's files. */
  const mentioned = createMemo(() => mentionedChips(text(), atItems()));
  createEffect(() => p.onAttachments?.(mentioned()));
  const [multiline, setMultiline] = createSignal(false);
  const resize = () => {
    ta.style.height = "0px";
    ta.style.minHeight = "0";
    setMultiline(text().includes("\n") || ta.scrollHeight > 44);
    ta.style.height = Math.min(ta.scrollHeight, 220) + "px";
    ta.style.minHeight = "";
  };

  createEffect(() => { const revision = p.revision; if (revision) { setText(revision.text); queueMicrotask(() => { resize(); ta?.focus(); }); } });
  const draftOf = (visible: string, m = mode()) => modePrompt(m, visible);
  const write = (visible: string, nextMode = mode()) => {
    ta.value = visible;
    setText(visible);
    p.onDraft?.(draftOf(visible, nextMode));
    resize();
  };
  const enterMode = (next: ComposerMode, visible: string) => {
    setMode(next);
    write(visible, next);
    ta.focus();
  };
  const clearMode = () => {
    setMode(null);
    p.onDraft?.(text());
    ta.focus();
  };
  const pickSlash = (item: { id: ComposerMode }) => enterMode(item.id, "");
  /**
   * Write the chosen attachment into the sentence at the point `@` was typed.
   *
   * The mention is the reference: there is no separate list to keep in step, so
   * editing the text is editing the attachments, and the agent reads the same
   * words the user wrote.
   */
  const pickAttach = (item: AttachItem) => {
    const next = insertMention(text(), caret(), item);
    write(next.text);
    setAtOff(true);
    ta.focus();
    ta.setSelectionRange(next.caret, next.caret);
    setCaret(next.caret);
  };
  const send = async () => {
    const original = text();
    let payload = draftOf(original);
    if ((!payload && !images().length) || sending() || uploading() || readingImages()) return;
    const sentImages = images();
    const originalMode = mode();
    const originalUploads = uploaded;
    const draftScope = p.scope;
    const project = p.projectSlug;
    const destination = uploadTarget();
    setPendingSend({text:original,images:sentImages});
    setSending(true);
    setText(""); setImages([]); ta.value=""; resize();
    let accepted = false;
    try {
      await attachmentLoad;
      if (project !== p.projectSlug || draftScope !== p.scope || destination !== uploadTarget()) return;
      const resolved = await prepareUploads(original, originalUploads, destination);
      if (project !== p.projectSlug || draftScope !== p.scope || destination !== uploadTarget()) return;
      uploaded = resolved.files;
      payload = draftOf(resolved.text);
      p.onAttachments?.(mentionedChips(resolved.text,atItems()));
      accepted = await p.onSend(payload || "Please look at the attached images.", sentImages) !== false;
      if (accepted) {
        uploaded = [];
        setUploadError(null);
        if (draftScope) await saveComposerDraft(draftScope, {text:"",images:[]}).catch(() => {});
        setMode(null);
      }
    } catch (error) { setUploadError(error instanceof Error ? error.message : String(error)); }
    finally {
      if (!accepted && draftScope === p.scope) {
        uploaded = originalUploads;
        setMode(originalMode);setText(original);setImages(sentImages);
        ta.value=original;resize();if(ta.isConnected)ta.focus();
      }
      setPendingSend(null);setSending(false);
    }
  };
  const insertDictation = (t: string) => {
    const cur = ta.value;
    const { value, caret } = insertAtCaret(cur, ta.selectionStart ?? cur.length, ta.selectionEnd ?? cur.length, t);
    write(value);
    ta.setSelectionRange(caret, caret);
    ta.focus();
  };
  onMount(() => p.autofocus && ta.focus());
  const close = () => {
    setMenu(false);
    setCtx(false);
    setWhich(null);
    if (slash()) setSlashOff(true);
    if (at()) setAtOff(true);
  };
  const onEsc = (e: KeyboardEvent) => {
    if (e.defaultPrevented || hasFocusScope()) return;
    // `close()` already dismisses the `@` picker; it was missing from this
    // guard, so Escape did nothing while only that picker was open.
    if (e.key === "Escape" && (menu() || ctx() || which() || slash() || at())) {
      e.stopPropagation();
      close();
    }
  };
  onMount(() => {
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", onEsc, true);
  });
  onCleanup(() => {
    window.removeEventListener("pointerdown", close);
    window.removeEventListener("keydown", onEsc, true);
  });
  const plus = (
    <div class="plus-wrap" onPointerDown={(e) => e.stopPropagation()}>
      <input ref={fileInput} type="file" multiple hidden aria-label="Choose files or images" onChange={(event) => { const files = Array.from(event.currentTarget.files ?? []); event.currentTarget.value = ""; void attachSources(files.map(file => ({ name: file.name, file }))); }} />
      <button class="plus" title="Add context" disabled={uploading() || sending()} onClick={() => setCtx(!ctx())}>
        <Ic.PlusIcon size={14} />
      </button>
      <Show when={ctx()}>
        <div class="menu plus-menu slash-menu">
          <button class="menu-item" onClick={chooseFiles}><span class="menu-ico"><Ic.FileIcon size={14} /></span>Upload file or image…</button>
          <Show when={atItems().some((i) => i.section === "Controller")}>
            <div class="slash-head">Controller</div>
            <For each={atItems().filter((i) => i.section === "Controller")}>
              {(it) => (
                <button class={`menu-item ${mentioned().some((c) => c.id === it.id) ? "on" : ""}`} onClick={() => pickAttach(it)}>
                  <span class="menu-ico"><Ic.TargetIcon size={14} /></span> {it.label}
                </button>
              )}
            </For>
          </Show>
          <Show when={atItems().some((i) => i.section === "Folders") || (p.files?.length ?? 0) > 0}>
            <div class="slash-head">Folders</div>
            <For each={atItems().filter((i) => i.section === "Folders")}>
              {(it) => (
                <button class={`menu-item ${mentioned().some((c) => c.id === it.id) ? "on" : ""}`} onClick={() => pickAttach(it)}>
                  <span class="menu-ico"><Ic.FolderIcon size={14} /></span> {it.label}
                </button>
              )}
            </For>
          </Show>
          <div class="slash-head">Files</div>
          <For each={isConnected() ? atItems().filter((i) => i.section === "Files") : p.files}>
            {(f) => {
              const id = "id" in f ? f.id : (f as { id: string }).id;
              const label = "label" in f ? (f as AttachItem).label : (f as { name: string }).name;
              const item: AttachItem = "section" in f ? (f as AttachItem) : { id, kind: "file", section: "Files", path: label, label };
              return (
                <button
                  class={`menu-item ${mentioned().some((c) => c.id === item.id) || p.attached?.includes(item.id) ? "on" : ""}`}
                  onClick={() => {
                    if (p.onAttachments) pickAttach(item);
                    else p.onToggleFile?.(item.id);
                  }}
                >
                  <span class="menu-ico"><Ic.FileIcon size={14} /></span> {label}
                </button>
              );
            }}
          </For>
        </div>
      </Show>
    </div>
  );
  // Two pickers, Cursor-style: harness first (Claude Code, Codex, …), then
  // the model that harness can run. Changing the harness resets the model.
  const choice = () => harnessChoices().find((c) => c.backend.id === pick()?.backend);
  const modelLabel = () => {
    const m = choice()?.models.find((x) => x.value === pick()?.model);
    return m ? shortModelLabel(m.label) : (pick()?.model ?? "Model");
  };
  const modelBtn = (
    <Show when={p.picker !== false}>
      <div class="picks" onPointerDown={(e) => e.stopPropagation()}>
        <Show
          when={live()}
          fallback={
            <div class="model-wrap">
              <button class="model" onClick={() => setMenu(!menu())}>
                {model()} <Ic.ChevronDown size={12} />
              </button>
              <Show when={menu()}>
                <div class="menu">
                  <For each={MODELS}>
                    {(m) => (
                      <button
                        class={`menu-item ${m === model() ? "on" : ""}`}
                        onClick={() => {
                          setModel(m);
                          setMenu(false);
                        }}
                      >
                        {m}
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          }
        >
          <div class="model-wrap">
            <button class={`model ${which() === "harness" ? "on" : ""}`} title="Harness" onClick={() => setWhich(which() === "harness" ? null : "harness")}>
              {choice()?.backend.name ?? "Harness"} <Ic.ChevronDown size={12} />
            </button>
            <Show when={which() === "harness"}>
              <div class="menu">
                <For each={harnessChoices().filter((c) => !p.harnessIds || p.harnessIds.includes(c.backend.id))}>
                  {(c) => (
                    <button
                      class={`menu-item ${c.backend.id === pick()?.backend ? "on" : ""}`}
                      onClick={() => {
                        // Switching harness resets the model, and with it the
                        // effort: the new harness may not accept any, or may
                        // not accept the level that was selected.
                        if (c.backend.id !== pick()?.backend) {
                          const effort = normalizeEffort(pick()?.effort, c.backend.id);
                          setHarnessPick({ backend: c.backend.id, model: c.models[0].value, ...(effort ? { effort } : {}) });
                        }
                        setWhich(null);
                      }}
                    >
                      <span class="pick-name">{c.backend.name}</span>
                      <span class={`pick-meta ${p.remoteSupport?.(c.backend.id).state ?? ""}`}>{p.remoteSupport?.(c.backend.id).note || c.models.length}</span>
                      <span class="pick-check">{c.backend.id === pick()?.backend ? "✓" : ""}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
          <span class="picks-sep">·</span>
          <div class="model-wrap">
            <button class={`model ${which() === "model" ? "on" : ""}`} title="Model" onClick={() => setWhich(which() === "model" ? null : "model")}>
              {modelLabel()} <Ic.ChevronDown size={12} />
            </button>
            <Show when={which() === "model"}>
              <div
                class="menu model-menu"
                ref={(el) => {
                  // Fresh element each open: start at the top, then keep the
                  // current model in view without jumping past the first rows.
                  el.scrollTop = 0;
                  requestAnimationFrame(() => el.querySelector(".menu-item.on")?.scrollIntoView({ block: "nearest" }));
                }}
              >
                <For each={choice()?.models ?? []}>
                  {(m) => (
                    <button
                      class={`menu-item ${m.value === pick()?.model ? "on" : ""}`}
                      title={m.value}
                      onClick={() => {
                        const backend = choice()!.backend.id;
                        const effort = normalizeEffort(pick()?.effort, backend);
                        setHarnessPick({ backend, model: m.value, ...(effort ? { effort } : {}) });
                        setWhich(null);
                      }}
                    >
                      <span class="pick-name">{shortModelLabel(m.label)}</span>
                      <span class="pick-check">{m.value === pick()?.model ? "✓" : ""}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
          {/* Effort, after harness and model. Only rendered for a harness the
              core actually accepts an effort for — everything else has
              model_effort forced to null server-side. */}
          <Show when={harnessSupportsEffort(pick()?.backend)}>
            <span class="picks-sep">·</span>
            <div class="model-wrap">
              <button
                class={`model ${which() === "effort" ? "on" : ""}`}
                title="Reasoning effort"
                aria-label={`Reasoning effort: ${effortLabel(pick()?.effort)}`}
                onClick={() => setWhich(which() === "effort" ? null : "effort")}
              >
                {effortLabel(pick()?.effort)} <Ic.ChevronDown size={12} />
              </button>
              <Show when={which() === "effort"}>
                <div class="menu">
                  <button
                    class={`menu-item ${!pick()?.effort ? "on" : ""}`}
                    title="Let the harness choose — no model_effort is sent"
                    onClick={() => {
                      const cur = pick()!;
                      setHarnessPick({ backend: cur.backend, model: cur.model });
                      setWhich(null);
                    }}
                  >
                    <span class="pick-name">{DEFAULT_EFFORT_LABEL}</span>
                    <span class="pick-check">{!pick()?.effort ? "✓" : ""}</span>
                  </button>
                  <For each={supportedEfforts(pick()?.backend)}>
                    {(e) => (
                      <button
                        class={`menu-item ${e === pick()?.effort ? "on" : ""}`}
                        onClick={() => {
                          const cur = pick()!;
                          setHarnessPick({ backend: cur.backend, model: cur.model, effort: e });
                          setWhich(null);
                        }}
                      >
                        <span class="pick-name">{effortLabel(e)}</span>
                        <span class="pick-check">{e === pick()?.effort ? "✓" : ""}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          </Show>
        </Show>
      </div>
    </Show>
  );
  // Cursor keeps the microphone mounted while a turn is streaming; Stop sits
  // beside it. A draft still sends — the backend queues it for the next turn.
  const sendBtn = (
    <div class="send-slot">
      <Show when={(text().trim() || images().length) && !slash() && !voiceActive()}>
        <button class="send" disabled={uploading() || sending() || readingImages()} onClick={send} title={p.busy ? "Queue for next turn" : "Send"}>
          <Ic.ArrowUpIcon size={14} />
        </button>
      </Show>
      <Show when={p.busy}>
        <button class="send stop" onClick={p.onStop} title="Stop">
          <Ic.StopIcon size={14} />
        </button>
      </Show>
    </div>
  );
  const voice = (
    <Show when={voiceAvailable()}>
      <div class="voice-slot" onPointerDown={(e) => e.stopPropagation()}>
        <VoiceButton scope={p.scope} onText={insertDictation} onActive={setVoiceActive} />
      </div>
    </Show>
  );
  const atMenu = (
    <Show when={at()}>
      {(s) => (
        <div class="menu slash-menu" role="listbox" aria-label="Context" onPointerDown={(e) => e.stopPropagation()}>
          <For each={["Controller", "Folders", "Files"] as const}>
            {(section) => {
              const rows = () => s().items.filter((it) => it.section === section);
              return (
                <Show when={rows().length}>
                  <div class="slash-head">{section}</div>
                  <For each={rows()}>
                    {(it) => {
                      const idx = () => s().items.indexOf(it);
                      return (
                        <button
                          type="button"
                          role="option"
                          aria-selected={atHi() === idx()}
                          class={`menu-item ${atHi() === idx() ? "on" : ""}`}
                          onMouseEnter={() => setAtHi(idx())}
                          onClick={() => pickAttach(it)}
                        >
                          <span class="menu-ico">{it.kind === "folder" ? <Ic.FolderIcon size={14} /> : it.kind === "controller" ? <Ic.TargetIcon size={14} /> : <Ic.FileIcon size={14} />}</span>
                          {it.label}
                        </button>
                      );
                    }}
                  </For>
                </Show>
              );
            }}
          </For>
        </div>
      )}
    </Show>
  );
  const slashMenu = (
    <Show when={slash()}>
      {(s) => (
        <div class="menu slash-menu" role="listbox" aria-label="Commands" onPointerDown={(e) => e.stopPropagation()}>
          <div class="slash-head">Modes</div>
          <For each={s().items}>
            {(it, i) => (
              <button
                type="button"
                role="option"
                aria-selected={slashHi() === i()}
                class={`menu-item ${slashHi() === i() ? "on" : ""}`}
                title={it.title}
                onMouseEnter={() => setSlashHi(i())}
                onClick={() => pickSlash(it)}
              >
                <span class="menu-ico"><Ic.TargetIcon size={14} /></span>
                {it.label}
              </button>
            )}
          </For>
        </div>
      )}
    </Show>
  );
  return (<>
    <Show when={pendingSend()}>{pending=><div class="composer-pending" aria-label="Pending message"><div class="user pending"><Show when={pending().images.length}><div class="message-images"><For each={pending().images}>{(image,index)=><div class="message-image"><img src={image.dataUrl} alt={`Image #${index()+1}`}/><span>#{index()+1}</span></div>}</For></div></Show><span>{pending().text}</span></div><span class="composer-pending-status" role="status">Sending…</span></div>}</Show>
    <div class={`composer ${p.tall || images().length || multiline() ? "tall" : ""} ${voiceActive() ? "voice-on" : ""} ${mode() ? "has-mode" : ""}`} data-mode={mode() ?? ""} onClick={() => !voiceActive() && ta.focus()}>
      {plus}
      {slashMenu}
      {atMenu}
      <Show when={uploading()}><div class="composer-upload-status" role="status">Attaching file…</div></Show>
      <Show when={uploadError()}><div class="composer-upload-status error" role="alert">{uploadError()}</div></Show>
      <Show when={mode() === "goal"}><ModeChip mode="goal" onClear={clearMode} /></Show>
      <div class="composer-field">
        <Show when={images().length}><div class="composer-images"><For each={images()}>{image => <div class="composer-image"><img src={image.dataUrl} alt="Attached image" /><button class="icon-btn" aria-label="Remove image" title="Remove image" onClick={e => { e.stopPropagation(); setImages(current => current.filter(item => item.id !== image.id)); }}><Ic.CloseIcon size={12}/></button></div>}</For></div></Show>
        <Show when={imageError()}><span class="image-paste-error" role="alert">{imageError()}</span></Show>
        <textarea readOnly={sending()}
          ref={ta}
          onPaste={event => void pasteImages(event)}
          rows={1}
          placeholder={mode() === "goal" ? "Describe the objective" : p.placeholder}
          onInput={(e) => {
            const next = e.currentTarget.value;
            setSlashOff(false);
            setAtOff(false);
            setCaret(e.currentTarget.selectionStart ?? next.length);
            if (!mode() || mode() === "goal") {
              const absorbed = absorbGoalPrefix(next);
              if (absorbed !== null && modes().some((it) => it.id === "goal")) {
                enterMode("goal", absorbed);
                return;
              }
            }
            write(next);
          }}
          onClick={() => setCaret(ta.selectionStart ?? text().length)}
          onKeyUp={() => setCaret(ta.selectionStart ?? text().length)}
          onKeyDown={(e) => {
            const atItemsOpen = at()?.items;
            if (atItemsOpen && atItemsOpen.length) {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                const n = atItemsOpen.length;
                setAtHi((i) => (e.key === "ArrowDown" ? (i + 1) % n : (i - 1 + n) % n));
                return;
              }
              if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                pickAttach(atItemsOpen[Math.min(atHi(), atItemsOpen.length - 1)]);
                return;
              }
            }
            const items = slash()?.items;
            if (items) {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                const n = items.length;
                setSlashHi((i) => (e.key === "ArrowDown" ? (i + 1) % n : (i - 1 + n) % n));
                return;
              }
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                pickSlash(items[Math.min(slashHi(), items.length - 1)]);
                return;
              }
              if (e.key === "Tab") {
                e.preventDefault();
                pickSlash(items[Math.min(slashHi(), items.length - 1)]);
                return;
              }
            }
            if (e.key === "Backspace" && mode() === "goal" && !text() && ta.selectionStart === 0 && ta.selectionEnd === 0) {
              e.preventDefault();
              clearMode();
              return;
            }
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              send();
            }
          }}
        />
      </div>
      {modelBtn}
      {voice}
      {sendBtn}
    </div>
  </>);
}

export default function App() {
  const [projects, setProjects] = createStore(structuredClone(seed));
  const [selected, setSelected] = createSignal<string | null>(localStorage.getItem("orb.selectedConversation") === "" ? null : localStorage.getItem("orb.selectedConversation") || "a1");
  createEffect(() => { localStorage.setItem("orb.selectedConversation",selected() ?? ""); });
  const [collapsed, setCollapsed] = createStore<Record<string, boolean>>({});
  const [sidebar, setSidebar] = createSignal(!window.matchMedia("(max-width: 720px)").matches);
  const [sbWidth, setSbWidth] = createSignal(220);
  const [streamingId, setStreamingId] = createSignal<string | null>(null);
  const [newFolder, setNewFolder] = createSignal<{ project: string; path: string } | null>(null);
  const folderTags = (project: string | null | undefined) => newFolder()?.project === project && newFolder()?.path ? [`orb-folder:${newFolder()!.path}`] : [];
  const [newProject, setNewProject] = createSignal(seed[0].id);
  // "New project…" inside the project picker (Cursor puts creation at the
  // bottom of the picker it belongs to, never in the sidebar chrome).
  const [newProjectDraft, setNewProjectDraft] = createSignal(false);
  createEffect(on(projectsVersion, () => {
    if (isConnected()) listProjects().then(setLiveProjects).catch(() => {});
  }, { defer: true }));
  const submitNewProject = async (title: string, slug: string) => {
    await createProject({ slug, title });
    setLiveProjects((current) => [...current, { slug, title } as ProjectSummary]);
    bumpProjects();
    setNewProject(slug);
    setNewProjectDraft(false);
  };
  const [liveProjects, setLiveProjects] = createSignal<ProjectSummary[]>([]);
  const effectiveNewProject = createMemo(() => isConnected()
    ? (liveProjects().find(p => p.slug === newProject())?.slug ?? liveProjects()[0]?.slug)
    : newProject());
  const [createError, setCreateError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [launchPreview, setLaunchPreview] = createSignal<LaunchReceipt | null>(null);
  let launchAttempt: { signature: string; key: string } | undefined;
  const MACHINE_KEY = "orb.machine";
  const [newMachine, setNewMachine] = createSignal(localStorage.getItem(MACHINE_KEY) || "core");
  const chooseMachine = (id: string) => {
    setNewMachine(id);
    try { localStorage.setItem(MACHINE_KEY, id); } catch { /* ignore */ }
    if (id === "local") void refreshLocalAgents();
  };
  const [envOpen, setEnvOpen] = createSignal<"machine" | "project" | null>(null);
  const [history, setHistory] = createSignal<(string | null)[]>([selected()]);
  const [hIdx, setHIdx] = createSignal(0);

  const [plusFor, setPlusFor] = createSignal<string | null>(null);
  const [nameDlg, setNameDlg] = createSignal<null | { kind: "folder" | "file" | "rename-project" | "rename-folder" | "rename-file" | "rename-agent"; pid: string; fid?: string; fileId?: string; agentId?: string; value: string }>(null);
  const [attached, setAttached] = createSignal<string[]>([]);
  const [attachChips, setAttachChips] = createSignal<AttachChip[]>([]);
  const [ctx, setCtx] = createSignal<{ x: number; y: number; items: MenuEntry[] } | null>(null);
  /** A structured launch refusal, when the core gave one we can act on. */
  const [createRefusal, setCreateRefusal] = createSignal<{ refusal: LaunchRefusal; project: string | null } | null>(null);
  // Shared with ProjectFileView so ⌘/ reaches core-hosted reference files too.
  const mdSrc = mdSource;
  const setMdSrc = setMdSource;
  let scroller: HTMLDivElement | undefined;
  let timer: number | undefined;
  let prevStatus: Agent["status"] = "idle";

  const current = createMemo(() => {
    const id = selected();
    if (!id || PAGES.has(id) || id.startsWith("f:")) return null;
    for (const p of projects) for (const a of p.agents) if (a.id === id) return a;
    return null;
  });

  const [missions, setMissions] = createSignal<Mission[]>([]);
  const [previewContext, setPreviewContext] = createSignal<{id: string; pct: number | null} | null>(null);
  const [openMission, setOpenMission] = createSignal<Mission | null>(null);
  /** Only missions still doing something: the sidebar is a place to act,
   * not a history. Everything else lives under its project. */
  const [fleetNodes, setFleetNodes] = createSignal<RemoteNodeView[]>([]);
  /** Typed remote launch support as last advertised by the server. "loading"
   * until the first answer; "error" keeps the last capability but says so. */
  const [remoteLaunch, setRemoteLaunch] = createSignal<{ state: "loading" | "ready" | "error"; capability: RemoteLaunchCapability | null }>({ state: "loading", capability: null });
  const acceptFleet = (fleet: RemoteNodesResponse) => {
    setFleetNodes(fleet.nodes);
    setRemoteLaunch({ state: "ready", capability: fleet.remote_launch ?? null });
  };
  const harnessName = (id: string) => harnessChoices().find((c) => c.backend.id === id)?.backend.name ?? id;
  /** What the harness menu shows next to each harness for the selected machine. */
  const remoteSupport = (backend: string): { state: RemoteSupport; note: string } => {
    const machine = newMachine();
    if (!isConnected() || machine === "core") return { state: "unknown", note: "" };
    const rl = remoteLaunch();
    if (rl.state === "loading") return { state: "unknown", note: `checking ${nodeLabel(machine)}…` };
    const support = remoteHarnessSupport(rl.capability, backend);
    if (support === "supported") return { state: "supported", note: "" };
    if (support === "unsupported") return { state: "unsupported", note: `not on ${nodeLabel(machine)}` };
    return { state: "unknown", note: rl.state === "error" ? "remote support unknown" : "no typed remote launch" };
  };
  /** One-line remote launch summary for a node row in the machine menu. */
  const nodeLaunchNote = () => {
    const rl = remoteLaunch();
    if (rl.state === "loading") return "checking launch support…";
    const cap = rl.capability;
    if (!cap || cap.typed !== true) return rl.state === "error" ? "launch support unknown" : "no typed remote launch";
    const names = (cap.harnesses ?? []).map(harnessName);
    const summary = names.length ? names.join(", ") : "no harness enabled";
    return rl.state === "error" ? `${summary} (last known)` : summary;
  };
  const refreshMissions = async () => {
    try {
      const fresh = await listMissions();
      setMissions((prev) => mergeById(prev, fresh));
      const live = new Set(["active", "running", "pending", "queued", "starting", "resuming"]);
      for (const m of fresh) if (live.has(m.status)) prefetchTranscript(m.id);
      for (const key of cacheRecents()) {
        if (key.startsWith("m:")) prefetchTranscript(key.slice(2));
      }
    } catch {
      /* keep last good list */
    }
  };
  const refreshFleet = async () => {
    try {
      acceptFleet(await getRemoteNodes());
    } catch {
      /* keep last good list, but stop claiming the capability is current */
      setRemoteLaunch((prev) => ({ ...prev, state: "error" }));
    }
  };
  const currentMissionId = createMemo(() => {
    const id = selected();
    return id && id.startsWith("m:") ? id.slice(2) : null;
  });
  // Project controller (Hermes cron): `c:<slug>`.
  const currentController = createMemo(() => {
    const id = selected();
    if (id?.startsWith("c:")) return { slug: id.slice(2) };
    if (id?.startsWith("pc:")) {
      const [, slug, cronId] = id.split(":");
      return slug && cronId ? { slug, id: cronId } : null;
    }
    return null;
  });
  // Project settings page: `ps:<slug>`.
  const currentProjectSettings = createMemo(() => {
    const id = selected();
    return id?.startsWith("ps:") ? id.slice(3) : null;
  });
  // Hosted project file: `pf:<slug>:<path>` (path may itself contain slashes).
  const currentProjectFile = createMemo(() => {
    const id = selected();
    if (!id || !id.startsWith("pf:")) return null;
    const rest = id.slice(3);
    const sep = rest.indexOf(":");
    if (sep < 0) return null;
    return { slug: rest.slice(0, sep), path: rest.slice(sep + 1) };
  });
  const missionGlyph = (s: string): Agent["status"] =>
    s === "active" || s === "running"
      ? "running"
      : s === "failed" || s === "not_feasible" || s === "blocked" || s === "interrupted"
        ? "pr-closed"
        : "idle";
  const sortedNodes = () => [...fleetNodes()].sort((a, b) => Number(b.status === "online") - Number(a.status === "online"));
  const machineLabel = () => {
    if (isConnected()) {
      if (newMachine() === "core") return "Core (agent-core)";
      return nodeLabel(newMachine());
    }
    return MACHINES.find((m) => m.id === newMachine())?.name ?? nodeLabel(newMachine());
  };
  // Keyed on the connection only: the body reads selected()/newMachine()/
  // fleetNodes(), and tracking those made every fleet poll re-run the
  // effect, which re-polled the fleet — an endless fetch loop.
  createEffect(on(isConnected, (connected) => {
    if (connected) {
      void refreshMissions();
      void refreshFleet();
      void refreshHarnessChoices();
      listProjects()
        .then(setLiveProjects)
        .catch(() => setLiveProjects([]));
      // Seed agent ids only exist offline — don't land on a demo transcript.
      const sel = selected();
      if (sel && !sel.includes(":") && !PAGES.has(sel)) open(null);
      if (newMachine() === "local") void refreshLocalAgents();
    } else {
      // Backend views (missions, hosted files) can't render offline — e.g.
      // after a 401 cleared the token mid-session.
      const sel = selected();
      if (sel && (sel.startsWith("m:") || sel.startsWith("pf:") || sel.startsWith("c:") || sel.startsWith("pc:") || sel.startsWith("ps:") || sel === "execution")) open(null);
      // Keep an explicit machine selection across reconnects.
    }
  }));
  const currentFile = createMemo(() => {
    const id = selected();
    if (!id?.startsWith("f:")) return null;
    const [, pid, fid, fileId] = id.split(":");
    const p = projects.find((x) => x.id === pid);
    const folder = p?.folders.find((f) => f.id === fid);
    const file = folder?.files.find((f) => f.id === fileId);
    if (!p || !folder || !file) return null;
    return { pid, fid, file };
  });
  const projectFiles = createMemo(() => {
    if (isConnected()) return [];
    const id = selected();
    let pid: string | undefined;
    if (id?.startsWith("f:")) pid = id.split(":")[1];
    else if (id && !PAGES.has(id)) {
      for (const p of projects) if (p.agents.some((a) => a.id === id)) pid = p.id;
    }
    if (!pid) pid = newProject();
    const p = projects.find((x) => x.id === pid);
    if (!p) return [] as { id: string; name: string; text: string }[];
    return p.folders.flatMap((f) => f.files.map((file) => ({ id: `f:${p.id}:${f.id}:${file.id}`, name: `${f.name}/${file.name}`, text: file.text })));
  });
  const sessionPreview = createMemo<SessionPreviewData>(() => {
    const id = currentMissionId() ?? "";
    const mission = openMission()?.id === id ? openMission() : missions().find(m => m.id === id);
    const binding = localBinding(id);
    const local = !!binding || !!mission?.tags?.includes("placement:client");
    const backend = mission?.backend || binding?.harness;
    const choice = harnessChoices().find(c => c.backend.id === backend);
    const model = mission?.model_override || binding?.model;
    const modelLabel = choice?.models.find(m => m.value === model)?.label;
    const effort = normalizeEffort(mission?.model_effort, backend);
    return { id, title: displayTitle(mission?.title) || "Mission", local,
      destination: local ? "This computer" : missionDestination(mission ?? null, recalledLaunch(id)),
      directory: binding?.cwd || mission?.working_directory,
      project: liveProjects().find(p => p.slug === mission?.project)?.title || mission?.project,
      harness: choice?.backend.name || backend,
      model: modelLabel ? shortModelLabel(modelLabel) : model || undefined,
      effort: effort ? effortLabel(effort) : undefined,
      context: previewContext()?.id === id ? previewContext()?.pct : null };
  });

  const onSettings = () => selected() === "settings" || selected() === "routing";
  const openSettings = () => {
    open("settings");
  };
  const leaveSettings = () => {
    const h = history();
    for (let i = hIdx() - 1; i >= 0; i--) {
      if (h[i] !== "settings" && h[i] !== "routing") {
        if (selected() === "routing" && !confirmLeaveRouting(() => { if (open(h[i], false)) setHIdx(i); })) return;
        if (open(h[i], false)) setHIdx(i);
        return;
      }
    }
    open("a1");
  };

  const toBottom = (smooth = false) =>
    requestAnimationFrame(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: smooth ? "smooth" : "auto" }));

  const open = (id: string | null, push = true) => {
    if (selected() === "routing" && id !== "routing" && !confirmLeaveRouting(() => open(id, push))) return false;
    if (window.matchMedia("(max-width: 720px)").matches) setSidebar(false);
    batch(() => {
      setSelected(id);
      if (push) {
        const h = history().slice(0, hIdx() + 1);
        h.push(id);
        setHistory(h);
        setHIdx(h.length - 1);
      }
    });
    if (id?.startsWith("m:")) void loadTranscript(id.slice(2)).catch(() => {});
    toBottom();
    return true;
  };
  const nav = (d: number) => {
    const i = hIdx() + d;
    if (i < 0 || i >= history().length) return;
    if (selected() === "routing" && history()[i] !== "routing" && !confirmLeaveRouting(() => { if (open(history()[i], false)) setHIdx(i); })) return;
    if (open(history()[i], false)) setHIdx(i);
  };

  const stop = () => {
    clearInterval(timer);
    const id = streamingId();
    if (id) mutate(id, (a) => (a.status = prevStatus));
    setStreamingId(null);
  };

  const mutate = (id: string, fn: (a: Agent) => void) =>
    setProjects(
      produce((ps) => {
        for (const p of ps) for (const a of p.agents) if (a.id === id) fn(a);
      }),
    );

  const stream = (id: string) => {
    stop();
    setStreamingId(id);
    mutate(id, (a) => {
      prevStatus = a.status === "running" ? "idle" : a.status;
      a.status = "running";
      a.turns.push({ role: "agent", worked: "0s", blocks: [{ kind: "p", text: "" }] });
    });
    const words = LOREM_REPLY.split(" ");
    let i = 0;
    const t0 = performance.now();
    timer = window.setInterval(() => {
      i += 2;
      mutate(id, (a) => {
        const turn = a.turns[a.turns.length - 1];
        if (turn.role !== "agent") return;
        turn.blocks[0] = { kind: "p", text: words.slice(0, i).join(" ") };
        turn.worked = `${Math.max(1, Math.round((performance.now() - t0) / 1000))}s`;
      });
      const s = scroller;
      if (s && s.scrollHeight - s.scrollTop - s.clientHeight < 80) s.scrollTop = s.scrollHeight;
      if (i >= words.length) stop();
    }, 45);
  };

  const send = (text: string) => {
    const c = current();
    if (!c) return;
    const extra = attached()
      .map((id) => projectFiles().find((f) => f.id === id))
      .filter((f): f is { id: string; name: string; text: string } => !!f)
      .map((f) => `[${f.name}]\n${f.text}`)
      .join("\n\n");
    mutate(c.id, (a) => a.turns.push({ role: "user", text: extra ? `${extra}\n\n${text}` : text }));
    setAttached([]);
    toBottom(true);
    stream(c.id);
  };

  const finishLocal = async (id: string) => {
    try {
      const state = await followLocal(id, () => {});
      if (state.text.trim()) await appendClientTranscript(id, "assistant", state.text);
      const failed = (state.exit_code != null && state.exit_code !== 0) || (!!state.error && !state.text.trim());
      if (failed) recordLocalFailure(id, state.error || `Local process exited with code ${state.exit_code}`);
      await setClientMissionStatus(id, failed ? "failed" : "awaiting_user");
    } catch (error) {
      recordLocalFailure(id, error);
      await setClientMissionStatus(id, "failed").catch(() => {});
    }
    void refreshMissions();
  };
  const launchLocal = async (typed: string, prompt: string, title: string, projectSlug: string | undefined, pick: HarnessPick, images: DraftImage[]) => {
    if (!projectSlug) throw new Error("Choose a project before starting on this computer. Your draft is kept.");
    const rows = await refreshLocalAgents();
    const row = rows.find((item) => item.id === pick.backend && item.installed && item.path);
    if (!row?.path) throw new Error("That CLI is not installed on this computer. Set its path in Settings → Local agents. Your draft is kept.");
    const plan = await materializeMentions(projectSlug, prompt, attachChips());
    const root = await localWorkspace(projectSlug);
    if (plan.files.length) await writeLocalFiles(root, plan.files);
    const imagePaths = await stageLocalImages(root, images);
    const sent = imagePrompt(bindWorkspace(plan.prompt, root), imagePaths);
    const effort = normalizeEffort(pick.effort, pick.backend);
    const body = { title, prompt: imagePrompt(typed, imagePaths), project: projectSlug, tags: folderTags(projectSlug), backend: pick.backend, model_override: pick.model, placement: "client" as const, ...(effort ? { model_effort: effort } : {}) };
    const signature = JSON.stringify(body);
    if (launchAttempt?.signature !== signature) launchAttempt = { signature, key: crypto.randomUUID() };
    const m = await createMission({ ...body, idempotency_key: launchAttempt.key });
    launchAttempt = undefined;
    setAttachChips([]);
    rememberBinding(m.id, { harness: pick.backend, bin: row.path, cwd: root, model: pick.model });
    const receipt = { prompt: typed, nodeId: "local", destination: "This computer" };
    rememberLaunch(m.id, receipt);
    setMissions((prev) => [m, ...prev.filter((old) => old.id !== m.id)]);
    open(`m:${m.id}`);
    await startLocal({ id: m.id, harness: pick.backend, bin: row.path, cwd: root, prompt: sent, model: pick.model, imagePaths });
    void finishLocal(m.id);
    void refreshMissions();
  };

  const create = async (text: string, images: DraftImage[] = []) => {
    if (isConnected()) {
      if (creating()) return false;
      const goal = goalDraft(text);
      if (goal.kind === "empty") { setCreateError(EMPTY_GOAL_ERROR); return false; }
      // Goal mode rides on the prompt (server contract): the canonical
      // `/goal <objective>` enters goal mode; the title is the objective.
      const prompt = goal.kind === "goal" ? goalPrompt(goal.objective) : text;
      const title = missionTitle(text);
      const machine = newMachine();
      const receipt = {prompt,nodeId:machine,destination:nodeLabel(machine)};
      const projectSlug = effectiveNewProject();
      const pick = effectivePick();
      setCreating(true); setCreateError(null); setCreateRefusal(null); setLaunchPreview(receipt);
      try {
        if (!pick || !harnessChoices().some(c => c.backend.id === pick.backend && c.models.some(m => m.value === pick.model))) throw new Error("Choose an available harness and model before starting. Your draft is kept.");
        if (machine === "local") {
          await launchLocal(text, prompt, title, projectSlug, pick, images);
          return true;
        }
        if (machine !== "core") {
          // Fresh capability read every time: the server decides which
          // harnesses a node can run. A read failure refuses rather than guesses.
          let fleet: RemoteNodesResponse;
          try { fleet = await getRemoteNodes(); }
          catch (e) { setRemoteLaunch(prev => ({ ...prev, state: "error" })); throw new Error(remoteLaunchUnconfirmed(machine, e)); }
          acceptFleet(fleet);
          const refusal = remoteLaunchPreflight(fleet, machine, pick, harnessName);
          if (refusal) throw new Error(refusal);
        }
        // `effectivePick` already dropped an effort this harness can't take, so
        // an omitted field means "backend default" rather than a stale level.
        const effort = normalizeEffort(pick.effort, pick.backend);
        const attachments = attachChips().map(chipToAttachment);
        const sentPrompt = imagePrompt(prompt, await stageRemoteImages(images, undefined, machine));
        const body = {title,prompt:sentPrompt,project:projectSlug,tags:folderTags(projectSlug),backend:pick.backend,model_override:pick.model,...(effort ? {model_effort:effort} : {}),...(machine === "core" ? {} : {remote_node_id:machine}),...(attachments.length ? {attachments} : {})};
        const signature = JSON.stringify(body);
        if (launchAttempt?.signature !== signature) launchAttempt = {signature,key:crypto.randomUUID()};
        const m = await createMission({...body,idempotency_key:launchAttempt.key});
        launchAttempt = undefined;
        setAttachChips([]);
        rememberLaunch(m.id, receipt);
        setMissions(prev => [m, ...prev.filter(old => old.id !== m.id)]);
        open(`m:${m.id}`);
        void refreshMissions();
        return true;
      } catch (e) {
        // The idempotency key is deliberately *not* cleared here: a retry of
        // the same draft reuses it, so a request the server actually accepted
        // before failing the response cannot become a second mission.
        const refusal = launchRefusal(e, projectSlug ?? null);
        setCreateError(refusal.message);
        setCreateRefusal(refusal.kind === "other" ? null : { refusal, project: projectSlug ?? null });
        // Returning false keeps the composer text exactly as typed.
        return false;
      } finally { setCreating(false); setLaunchPreview(null); }
    }
    if (newMachine() === "core" || !MACHINES.some(m => m.id === newMachine())) { setCreateError("Reconnect the backend before launching on the selected machine. Your draft is kept."); return false; }
    const id = "n" + Date.now();
    const title = missionTitle(text);
    const extra = attached()
      .map((fid) => projectFiles().find((f) => f.id === fid))
      .filter((f): f is { id: string; name: string; text: string } => !!f)
      .map((f) => `[${f.name}]\n${f.text}`)
      .join("\n\n");
    const body = extra ? `${extra}\n\n${text}` : text;
    setProjects(
      (p) => p.id === newProject(),
      "agents",
      (as) => [{ id, title, status: "idle", context: 2, turns: [{ role: "user", text: body }] } as Agent, ...as],
    );
    setAttached([]);
    setCollapsed(newProject(), false);
    open(id);
    stream(id);
  };

  const confirmName = () => {
    const d = nameDlg();
    if (!d) return;
    const name = d.value.trim();
    if (!name) return;
    if (d.kind === "folder") {
      const id = "fd" + Date.now();
      setProjects(
        (p) => p.id === d.pid,
        "folders",
        (fs) => [...fs, { id, name, files: [] }],
      );
      setCollapsed(d.pid, false);
    } else if (d.kind === "file" && d.fid) {
      const id = "fl" + Date.now();
      const fname = name.endsWith(".md") ? name : `${name}.md`;
      setProjects(
        produce((ps) => {
          const p = ps.find((x) => x.id === d.pid);
          const f = p?.folders.find((x) => x.id === d.fid);
          f?.files.push({ id, name: fname, text: "" });
        }),
      );
      setCollapsed(d.pid, false);
      setCollapsed(`${d.pid}/${d.fid}`, false);
      setMdSrc(true);
      open(`f:${d.pid}:${d.fid}:${id}`);
    } else if (d.kind === "rename-project") {
      setProjects((p) => p.id === d.pid, "name", name);
    } else if (d.kind === "rename-folder" && d.fid) {
      setProjects(
        produce((ps) => {
          const f = ps.find((x) => x.id === d.pid)?.folders.find((x) => x.id === d.fid);
          if (f) f.name = name;
        }),
      );
    } else if (d.kind === "rename-file" && d.fid && d.fileId) {
      const fname = name.endsWith(".md") ? name : `${name}.md`;
      setProjects(
        produce((ps) => {
          const file = ps.find((x) => x.id === d.pid)?.folders.find((x) => x.id === d.fid)?.files.find((x) => x.id === d.fileId);
          if (file) file.name = fname;
        }),
      );
    } else if (d.kind === "rename-agent" && d.agentId) {
      mutate(d.agentId, (a) => (a.title = name));
    }
    setNameDlg(null);
  };

  const showCtx = (e: MouseEvent, items: MenuEntry[]) => {
    e.preventDefault();
    e.stopPropagation();
    setPlusFor(null);
    setCtx({ x: e.clientX, y: e.clientY, items });
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.defaultPrevented || hasFocusScope()) return;
    if (e.key === "Escape" && sidebar() && window.matchMedia("(max-width: 720px)").matches) {
      e.preventDefault(); setSidebar(false); return;
    }
    if (nameDlg()) {
      if (e.key === "Escape") setNameDlg(null);
      return;
    }
    if (ctx() && e.key === "Escape") {
      e.preventDefault();
      setCtx(null);
      return;
    }
    if (e.key === "Escape") {
      if (onSettings()) {
        e.preventDefault();
        leaveSettings();
      }
      return;
    }
    if (e.metaKey && e.key === "/") {
      // Both kinds of Markdown file view: the local demo files and the
      // core-hosted reference files, which render through ProjectFileView.
      if (currentFile() || currentProjectFile()) {
        e.preventDefault();
        toggleMdSource();
      }
      return;
    }
    if (!e.metaKey) return;
    if (e.key === "n") (e.preventDefault(), open(null));
    else if (e.key === "b") (e.preventDefault(), setSidebar(!sidebar()));
    else if (e.key === ",") (e.preventDefault(), onSettings() ? leaveSettings() : openSettings());
    else if (e.key === "[") (e.preventDefault(), nav(-1));
    else if (e.key === "]") (e.preventDefault(), nav(1));
  };
  onMount(() => {
    window.addEventListener("keydown", onKey);
    const closePlus = () => {
      setPlusFor(null);
      setEnvOpen(null);
    };
    window.addEventListener("pointerdown", closePlus);
    onCleanup(() => window.removeEventListener("pointerdown", closePlus));
    const stopMissions = pollWhileVisible(() => (isConnected() ? refreshMissions() : undefined), 5000);
    const stopFleet = pollWhileVisible(() => (isConnected() ? refreshFleet() : undefined), 15000);
    onCleanup(() => {
      stopMissions();
      stopFleet();
    });
    toBottom();
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKey);
    clearInterval(timer);
  });

  const startResize = (e: PointerEvent) => {
    e.preventDefault();
    document.body.classList.add("resizing");
    const move = (ev: PointerEvent) => setSbWidth(Math.min(420, Math.max(180, ev.clientX)));
    const up = () => {
      document.body.classList.remove("resizing");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div class={`app ${sidebar() ? "" : "sb-hidden"}`} style={{ "--sb-w": `${sbWidth()}px` }}>
      <FilePanelProvider scope={{ mission: currentMissionId() ? (openMission()?.id === currentMissionId() ? openMission() : missions().find(m => m.id === currentMissionId())) : undefined, project: currentController()?.slug, controller: currentController()?.id }}>
      <button class="sidebar-backdrop" aria-label="Close sidebar" onClick={() => setSidebar(false)} tabIndex={-1} />
      <aside id="orb-sidebar" class="sidebar">
        <div class="sb-top" data-tauri-drag-region />
        <nav class="sb-scroll" onContextMenu={(e) => e.preventDefault()}>
          <Show
            when={onSettings()}
            fallback={
              <>
                <button class={`row ${selected() === null ? "active" : ""}`} onClick={() => open(null)}>
                  <span class="row-ico"><Ic.NewAgentIcon /></span>
                  <span class="row-label">New Agent</span>
                  <kbd>⌘N</kbd>
                </button>
                <button class={`row ${selected() === "machines" ? "active" : ""}`} onClick={() => open("machines")}>
                  <span class="row-ico"><Ic.MachinesIcon /></span>
                  <span class="row-label">Machines</span>
                </button>
                <button class={`row ${selected() === "providers" ? "active" : ""}`} onClick={() => open("providers")}>
                  <span class="row-ico"><Ic.ProvidersIcon /></span>
                  <span class="row-label">Providers</span>
                </button>

                <Show
                  when={isConnected()}
                  fallback={
                    <>
                      <div class="section">Projects</div>
                      <div class="sb-empty">
                        No backend connected.
                        <button class="sb-link" onClick={() => openSettings()}>
                          Connect
                        </button>
                      </div>
                    </>
                  }
                >
                  <LiveProjectsSection
                    harnessChoices={harnessChoices()}
                    onFork={m => { setMissions(ms => [m, ...ms.filter(x => x.id !== m.id)]); bumpProjects(); open(`m:${m.id}`); }}
                    selected={selected}
                    open={open}
                    missionGlyph={missionGlyph}
                    StatusGlyph={StatusGlyph}
                    onNewAgent={(slug, path) => {
                      setNewFolder(path ? { project: slug, path } : null);
                      setNewProject(slug);
                      open(null);
                    }}
                    onNewProject={() => {
                      open(null);
                      setEnvOpen("project");
                    }}
                  />
                </Show>

              </>
            }
          >
            <button class="row" onClick={leaveSettings}>
              <span class="row-ico"><Ic.ArrowLeft /></span>
              <span class="row-label">Back</span>
            </button>
            <div class="section">Settings</div>
            <button class={`row ${selected() === "settings" ? "active" : ""}`} onClick={() => open("settings")}><span class="row-label">Client</span></button>
            <button class={`row ${selected() === "routing" ? "active" : ""}`} onClick={() => open("routing")}><span class="row-label">Routing</span></button>
          </Show>
        </nav>
        <div class="sb-foot">
          <button
            class={`gear-btn ${onSettings() ? "on" : ""}`}
            title="Settings (⌘,)"
            onClick={() => (onSettings() ? undefined : openSettings())}
          >
            <Ic.GearIcon />
          </button>
        </div>
        <div class="sb-resize" onPointerDown={startResize} />
      </aside>

      <header class="titlebar" data-tauri-drag-region>
        <div class="tb-left">
          <button class="icon-btn" aria-label="Toggle sidebar" aria-expanded={sidebar()} aria-controls="orb-sidebar" title="Toggle sidebar (⌘B)" onClick={() => setSidebar(!sidebar())}>
            <Ic.SidebarIcon />
          </button>
          <button class="icon-btn" title="Search">
            <Ic.SearchIcon />
          </button>
          <span class="tb-gap" />
          <button class="icon-btn" disabled={hIdx() === 0} onClick={() => nav(-1)}>
            <Ic.ArrowLeft />
          </button>
          <button class="icon-btn" disabled={hIdx() >= history().length - 1} onClick={() => nav(1)}>
            <Ic.ArrowRight />
          </button>
        </div>
        <div class="tb-title" data-tauri-drag-region>
          <Switch>
            <Match when={selected() === "settings"}>
              <span>Settings · Client</span>
            </Match>
            <Match when={selected() === "routing"}><span>Settings · Routing</span></Match>
            <Match when={selected() === "machines"}>
              <span>Machines</span>
            </Match>
            <Match when={selected() === "providers"}>
              <span>Providers</span>
            </Match>
            <Match when={selected() === "execution"}>
              <span>Execution</span>
            </Match>
            <Match when={currentProjectSettings()}>
              {(slug) => <span>{liveProjects().find((x) => x.slug === slug())?.title ?? slug()} · Settings</span>}
            </Match>
            <Match when={currentMissionId()}>
              {(id) => (
                <>
                  <SessionPreview data={sessionPreview()} goal={!!missionGoal(missions().find((m) => m.id === id()) ?? openMission())} />
                </>
              )}
            </Match>
            <Match when={currentProjectFile()}>
              {(pf) => (
                <>
                  <span>{pf().path.split("/").pop()}</span>
                  <Ic.CloudIcon class="dim" />
                  <kbd class="tb-kbd">{mdSrc() ? "Preview" : "Source"} ⌘/</kbd>
                </>
              )}
            </Match>
            <Match when={currentFile()}>
              {(f) => (
                <>
                  <span>{f().file.name}</span>
                  <kbd class="tb-kbd">{mdSrc() ? "Preview" : "Source"} ⌘/</kbd>
                </>
              )}
            </Match>
            <Match when={current()}>
              {(c) => (
                <>
                  <span>{c().title}</span>
                  <Show when={c().cloud} fallback={<Ic.LaptopIcon class="dim" />}>
                    <Ic.CloudIcon class="dim" />
                  </Show>
                </>
              )}
            </Match>
            <Match when={true}>
              <span>New Agent</span>
            </Match>
          </Switch>
        </div>
        <FilePanelButton />
      </header>

      <main class="main">
        <Switch>
          <Match when={selected() === "settings"}>
            <Settings onOpenPage={open} />
          </Match>
          <Match when={selected() === "routing"}>
            <RoutingSettings onOpenClient={() => open("settings")} />
          </Match>
          <Match when={selected() === "machines"}>
            <Machines />
          </Match>
          <Match when={selected() === "providers"}>
            <Providers />
          </Match>
          <Match when={currentMissionId()}>
            {(id) => (
              <Show when={id()} keyed>
                {(mid) => <MissionView id={mid} initial={missions().find(m => m.id === mid)} onMission={setOpenMission} onContext={(id, pct) => setPreviewContext({id, pct})} onFork={m => { setMissions(ms => [m, ...ms.filter(x => x.id !== m.id)]); bumpProjects(); open(`m:${m.id}`); }} />}
              </Show>
            )}
          </Match>
          <Match when={selected() === "execution"}>
            <ExecutionSettings />
          </Match>
          <Match when={currentController()}>
            {(slug) => (
              <Show when={slug()} keyed>
                {(s) => <ControllerView slug={s.slug} id={s.id} />}
              </Show>
            )}
          </Match>
          <Match when={currentProjectSettings()}>
            {(slug) => (
              <Show when={slug()} keyed>
                {(s) => <ProjectSettings slug={s} onOpenPage={open} onOpenMission={(id) => open(`m:${id}`)} />}
              </Show>
            )}
          </Match>
          <Match when={currentProjectFile()}>
            {(pf) => (
              <Show when={pf()} keyed>
                {(f) => <ProjectFileView slug={f.slug} path={f.path} />}
              </Show>
            )}
          </Match>
          <Match when={currentFile()}>
            {(f) => {
              const save = (text: string) =>
                setProjects(
                  produce((ps) => {
                    const file = ps
                      .find((x) => x.id === f().pid)
                      ?.folders.find((x) => x.id === f().fid)
                      ?.files.find((x) => x.id === f().file.id);
                    if (file) file.text = text;
                  }),
                );
              return (
                <div class="file-view">
                  <Show when={mdSrc()} fallback={<MdView text={f().file.text} />}>
                    <MdSource text={f().file.text} onInput={save} />
                  </Show>
                </div>
              );
            }}
          </Match>
          <Match when={true}>
        <Show
          when={current()}
          keyed
          fallback={
            <div class={`new-agent ${creating() ? "launching" : ""}`}>
              <div class="new-inner">
                <div class="na-meta">
                  <div class="na-drop" onPointerDown={(e) => e.stopPropagation()}>
                    <button class="na-drop-btn" aria-label="Choose project" aria-haspopup="dialog" aria-expanded={envOpen() === "project"} onClick={() => setEnvOpen(envOpen() === "project" ? null : "project")}>
                      {isConnected()
                        ? (liveProjects().find((p) => p.slug === effectiveNewProject())?.title ?? effectiveNewProject() ?? "No project")
                        : projects.find((p) => p.id === newProject())?.name}
                      <Show when={newFolder()?.project === effectiveNewProject()}><span class="new-agent-folder">/ {newFolder()?.path}</span></Show>
                      <Ic.ChevronDown size={12} />
                    </button>
                    <Show when={envOpen() === "project"}>
                      <ProjectPicker projects={isConnected() ? [...liveProjects()].sort((a,b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "")).map((p) => ({ id: p.slug, name: p.title ?? p.slug })) : projects.map((p) => ({ id: p.id, name: p.name }))}
                        selected={effectiveNewProject() ?? ""} canCreate={isConnected()}
                        onSelect={(id) => { setNewFolder(null); setNewProject(id); setEnvOpen(null); }}
                        onClose={() => setEnvOpen(null)}
                        onCreate={() => { setEnvOpen(null); setNewProjectDraft(true); }}
                        onMachine={() => setEnvOpen("machine")} />
                    </Show>
                  </div>
                  <div class="na-drop" onPointerDown={(e) => e.stopPropagation()}>
                    <button class="na-drop-btn" onClick={() => setEnvOpen(envOpen() === "machine" ? null : "machine")}>
                      <Show when={newMachine() !== "local"} fallback={<Ic.LaptopIcon size={14} />}>
                        <Show when={newMachine() === "core"} fallback={<Ic.ComputeNodeIcon size={14} />}><Ic.CoreServerIcon size={14} /></Show>
                      </Show>
                      {machineLabel()}
                      <Ic.ChevronDown size={12} />
                    </button>
                    <Show when={envOpen() === "machine"}>
                      <div class="menu na-menu machine-menu">
                        <div class="na-menu-list">
                        <Show
                          when={isConnected()}
                          fallback={
                            <>
                              <For each={MACHINES.filter((m) => m.id === "local")}>
                                {(m) => (
                                  <button
                                    class={`menu-item ${m.id === newMachine() ? "on" : ""}`}
                                    onClick={() => {
                                      chooseMachine(m.id);
                                      setEnvOpen(null);
                                    }}
                                  >
                                    <span class="menu-ico">
                                      <Ic.LaptopIcon />
                                    </span>
                                    {m.name}
                                  </button>
                                )}
                              </For>
                              <div class="menu-sep" />
                              <For each={MACHINES.filter((m) => m.id !== "local")}>
                                {(m) => (
                                  <button
                                    class={`menu-item ${m.id === newMachine() ? "on" : ""}`}
                                    onClick={() => {
                                      chooseMachine(m.id);
                                      setEnvOpen(null);
                                    }}
                                  >
                                    <span class="menu-ico">
                                      <Ic.ComputeNodeIcon />
                                    </span>
                                    <span class="menu-col">
                                      <span class="menu-title">{m.name}</span>
                                      <span class="menu-sub">{m.user}@{m.host}</span>
                                    </span>
                                  </button>
                                )}
                              </For>
                            </>
                          }
                        >
                          <div class="machine-section-label">Local</div>
                          <button
                            class={`menu-item ${newMachine() === "local" ? "on" : ""}`}
                            onClick={() => {
                              chooseMachine("local");
                              setEnvOpen(null);
                            }}
                          >
                            <span class="menu-ico">
                              <Ic.LaptopIcon />
                            </span>
                            <span class="menu-col">
                              <span class="menu-title">This computer</span>
                              <span class="menu-sub">Installed agents on your Mac</span>
                            </span>
                          </button>
                          <div class="menu-sep" />
                          <div class="machine-section-label">Remote</div>
                          <button
                            class={`menu-item ${newMachine() === "core" ? "on" : ""}`}
                            onClick={() => {
                              chooseMachine("core");
                              setEnvOpen(null);
                            }}
                          >
                            <span class="menu-ico">
                              <Ic.CoreServerIcon />
                            </span>
                            <span class="menu-col">
                              <span class="menu-title">Core (agent-core)</span>
                              <span class="menu-sub">Control plane · agent-core</span>
                            </span>
                          </button>
                          <div class="machine-section-label machine-nodes-label">Compute nodes</div>
                          <For each={sortedNodes()}>
                            {(n) => (
                              <button
                                class={`menu-item ${n.id === newMachine() ? "on" : ""}`}
                                title={nodeLaunchNote()}
                                onClick={() => {
                                  chooseMachine(n.id);
                                  setEnvOpen(null);
                                }}
                              >
                                <span class="menu-ico">
                                  <Ic.ComputeNodeIcon />
                                </span>
                                <span class="menu-col">
                                  <span class="menu-title">{n.id}</span>
                                </span>
                                <span class="machine-node-state"><span class={`machine-state-dot ${n.status === "online" && !n.cordoned ? "online" : ""}`} />{n.cordoned ? "Cordoned" : n.status}</span>
                              </button>
                            )}
                          </For>
                        </Show>
                        </div>
                        <div class="na-menu-foot">
                          <div class="menu-sep" />
                          <button
                            class="menu-item"
                            onClick={() => {
                              setEnvOpen(null);
                              open("machines");
                            }}
                          >
                            <span class="menu-ico">
                              <Ic.GearIcon />
                            </span>
                            Manage machines
                          </button>
                        </div>
                      </div>
                    </Show>
                  </div>
                </div>
                {/* The optimistic window: the prompt appears immediately and animates while
    the request is in flight. No banner and no reserved space — LaunchStatus
    stays silent for a healthy launch and speaks only if it is refused. */}
                <Show when={launchPreview()}>{(receipt) => <div class="launch-preview"><UserTurn text={receipt().prompt} pending /><LaunchStatus submitting destination={receipt().destination} goal={goalObjective(receipt().prompt)} /><MissionPending destination={receipt().destination} label="Starting" /></div>}</Show>
                <div hidden={creating()}>
                <Composer
                  placeholder="Describe a task, / for commands, @ for context"
                  busy={creating()}
                  onSend={create}
                  onStop={stop}
                  onDraft={(text) => { if (createError() === EMPTY_GOAL_ERROR && goalDraft(text).kind !== "empty") setCreateError(null); }}
                  autofocus
                  tall
                  scope="new-agent"
                  uploadTarget={newMachine()}
                  remoteSupport={remoteSupport}
                  harnessIds={newMachine() === "local" ? installedIds() : undefined}
                  files={projectFiles()}
                  attached={attached()}
                  onToggleFile={(id) =>
                    setAttached(attached().includes(id) ? attached().filter((x) => x !== id) : [...attached(), id])
                  }
                  projectSlug={effectiveNewProject()}
                  onAttachments={setAttachChips}
                />
                <Show when={createError()}>
                  <ErrorNotice error={createError()!} title="Couldn’t start the mission" onDismiss={() => setCreateError(null)}>
                    <Show when={/GiB required.*GiB is free/.test(createError()!)}><button class="s-btn sm" onClick={() => setEnvOpen("machine")}>Choose machine</button></Show>
                    <Show when={createRefusal()}>
                      {(r) => (
                        <div class="launch-refusal-actions">
                          <Show when={r().refusal.kind === "project_cap" && r().project}>
                            <span class="launch-refusal-meta">
                              {(r().refusal as Extract<LaunchRefusal, { kind: "project_cap" }>).active} of{" "}
                              {(r().refusal as Extract<LaunchRefusal, { kind: "project_cap" }>).cap} unfinished
                            </span>
                          </Show>
                          <Show when={r().project}>
                            {(slug) => (
                              <button class="s-btn sm" onClick={() => open(`ps:${slug()}`)}>
                                Open project settings
                              </button>
                            )}
                          </Show>
                        </div>
                      )}
                    </Show>
                  </ErrorNotice>
                </Show>
                </div>
              </div>
            </div>
          }
        >
          {(c) => (
            <>
              <div class="scroll" ref={scroller}>
                <div class="col">
                  <For each={c.turns}>
                    {(turn, i) =>
                      turn.role === "user" ? (
                        <div class="user">
                          <span>{turn.text}</span>
                          <button class="icon-btn restore" title="Restore checkpoint">
                            <Ic.ReplyIcon />
                          </button>
                        </div>
                      ) : (
                        <AgentTurn turn={turn} streaming={streamingId() === c.id && i() === c.turns.length - 1} />
                      )
                    }
                  </For>
                </div>
              </div>
              <div class="dock">
                <div class="col">
                  <div class="actions">
                    <Show when={c.diff}>
                      <button class="pill">
                        Review <span class="c-green">+{c.diff}</span>
                      </button>
                    </Show>
                    <button class="pill">
                      Commit &amp; Push <Ic.ChevronDown size={12} />
                    </button>
                    <button class="pill round">
                      <Ic.DotsIcon size={14} />
                    </button>
                  </div>
                  <Show when={attached().length}>
                    <div class="attach-pills">
                      <For each={attached()}>
                        {(id) => {
                          const f = () => projectFiles().find((x) => x.id === id);
                          return (
                            <button class="pill on" onClick={() => setAttached(attached().filter((x) => x !== id))}>
                              <Ic.FileIcon size={12} /> {f()?.name} <Ic.CloseIcon size={12} />
                            </button>
                          );
                        }}
                      </For>
                    </div>
                  </Show>
                  <Composer
                    placeholder="Send follow-up"
                    picker={false}
                    busy={streamingId() === c.id}
                    onSend={send}
                    onStop={stop}
                    scope={c.id}
                    files={projectFiles()}
                    attached={attached()}
                    onToggleFile={(id) =>
                      setAttached(attached().includes(id) ? attached().filter((x) => x !== id) : [...attached(), id])
                    }
                  />
                  <div class="under">
                    <span class="ctx">
                      <Ic.ContextRing pct={c.context} /> {c.context}%
                    </span>
                  </div>
                </div>
              </div>
            </>
          )}
        </Show>
          </Match>
        </Switch>
      </main>
      <Show when={ctx()}>
        {(c) => <PopupMenu x={c().x} y={c().y} items={c().items} onClose={() => setCtx(null)} />}
      </Show>
      <Show when={newProjectDraft()}><ProjectCreation existingIds={liveProjects().map(p => p.slug)} onCreate={submitNewProject} onClose={() => setNewProjectDraft(false)} /></Show>
      <Show when={nameDlg()}>
        {(d) => (
          <PromptSheet
            title={d().kind.startsWith("rename") ? "Rename" : d().kind === "folder" ? "New folder" : "New file"}
            label="Name"
            placeholder={d().kind === "folder" ? "notes" : "note.md"}
            value={d().value}
            onInput={(value) => setNameDlg({ ...d(), value })}
            action={d().kind.startsWith("rename") ? "Save" : "Create"}
            disabled={!d().value.trim()}
            onAction={confirmName}
            onClose={() => setNameDlg(null)}
          />
        )}
      </Show>
      </FilePanelProvider>
    </div>
  );
}

function MissionDock(p: {
  mission: Mission | null;
  items: StreamItem[];
  destination: string;
  onMission?: (mission: Mission) => void;
  onError?: (message: string) => void;
  onFork?: (mission: Mission) => void;
}) {
  const [forkOpen, setForkOpen] = createSignal(false);
  const used = () => estimateTokens(p.items);
  const windowSize = () => contextWindow(p.mission?.backend);
  const pct = () => contextPct(used(), windowSize());
  const [open, setOpen] = createSignal(false);
  const [modelOpen, setModelOpen] = createSignal(false);
  const [effortOpen, setEffortOpen] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const choice = () => harnessChoices().find((c) => c.backend.id === p.mission?.backend);
  const harnessName = () => choice()?.backend.name ?? p.mission?.backend ?? "";
  const modelId = () => p.mission?.model_override || "";
  const modelLabel = () => {
    const id = modelId();
    const m = choice()?.models.find((x) => x.value === id);
    if (m) return dockModelLabel(harnessName(), shortModelLabel(m.label));
    return id ? dockModelLabel(harnessName(), shortModelLabel(id)) : "Default";
  };
  const idle = () => missionSettingsIdle(p.mission?.status);
  const models = () => choice()?.models ?? [];
  const canChangeModel = () => idle() && models().length > 1 && !saving();
  // The mission's own stored effort, normalized against its harness: a mission
  // created before an effort was offered simply has none.
  const effort = () => normalizeEffort(p.mission?.model_effort, p.mission?.backend);
  const efforts = () => supportedEfforts(p.mission?.backend);
  const canChangeEffort = () => idle() && efforts().length > 0 && !saving();
  const close = (e: PointerEvent) => {
    if (!(e.target instanceof Node)) return;
    const el = e.target as HTMLElement;
    if (!el.closest?.(".ctx-wrap")) setOpen(false);
    if (!el.closest?.(".under-model-wrap")) setModelOpen(false);
    if (!el.closest?.(".under-effort-wrap")) setEffortOpen(false);
  };
  const pickModel = async (value: string) => {
    const m = p.mission;
    if (!m || value === modelId() || saving()) { setModelOpen(false); return; }
    setSaving(true);
    setModelOpen(false);
    try {
      p.onMission?.(await updateMissionSettings(m.id, { model_override: value }));
    } catch (e) {
      p.onError?.(e instanceof Error && e.message.includes("409") ? "Stop the current turn before switching models." : launchError(e));
    } finally { setSaving(false); }
  };
  /** Next-turn effort. "" is the core's documented clear back to the backend
   * default (`normalize_string_patch` trims it to a null). */
  const pickEffort = async (value: string) => {
    const m = p.mission;
    if (!m || value === (effort() ?? "") || saving()) { setEffortOpen(false); return; }
    setSaving(true);
    setEffortOpen(false);
    try {
      p.onMission?.(await updateMissionSettings(m.id, { model_effort: value }));
    } catch (e) {
      p.onError?.(e instanceof Error && e.message.includes("409") ? "Stop the current turn before switching effort." : launchError(e));
    } finally { setSaving(false); }
  };
  onMount(() => window.addEventListener("pointerdown", close));
  onCleanup(() => window.removeEventListener("pointerdown", close));
  return (
    <div class="under">
      <span class="under-loc" title={p.destination}>
        <Show when={p.destination !== "Core"} fallback={<Ic.LaptopIcon size={13} />}>
          <Ic.CloudIcon />
        </Show>
        {p.destination}
      </span>
      <Show when={harnessName()}>
        <span class="under-sep" aria-hidden="true">·</span>
        <div class="fork-anchor"><button class="under-harness fork-trigger" title="Fork with another harness or model" aria-label="Fork conversation" onClick={() => setForkOpen(true)}>{harnessName()} <Ic.ChevronDown size={10} /></button>
        <Show when={forkOpen() && p.mission}>{m => <ForkMission mission={m()} choices={harnessChoices()} destination={p.destination} onClose={() => setForkOpen(false)} onFork={forked => { setForkOpen(false); p.onFork?.(forked); }} />}</Show></div>
        <span class="under-sep" aria-hidden="true">·</span>
        <div class="under-model-wrap">
          <Show
            when={canChangeModel()}
            fallback={
              <span class="under-model" title={idle() ? modelLabel() : "Stop the current turn to switch models"}>
                {modelLabel()}
              </span>
            }
          >
            <button
              class={`under-model ${modelOpen() ? "on" : ""}`}
              title="Model for the next turn"
              onClick={() => setModelOpen(!modelOpen())}
            >
              {modelLabel()} <Ic.ChevronDown size={10} />
            </button>
            <Show when={modelOpen()}>
              <div class="menu under-model-menu">
                <For each={models()}>
                  {(m) => (
                    <button
                      class={`menu-item ${m.value === modelId() ? "on" : ""}`}
                      onClick={() => pickModel(m.value)}
                    >
                      <span class="pick-name">{shortModelLabel(m.label)}</span>
                      <span class="pick-check">{m.value === modelId() ? "✓" : ""}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </div>
        <Show when={efforts().length > 0}>
          <span class="under-sep" aria-hidden="true">·</span>
          <div class="under-effort-wrap under-model-wrap">
            <Show
              when={canChangeEffort()}
              fallback={
                <span class="under-model" title={idle() ? `Effort: ${effortLabel(effort())}` : "Stop the current turn to switch effort"}>
                  {effortLabel(effort())}
                </span>
              }
            >
              <button
                class={`under-model ${effortOpen() ? "on" : ""}`}
                title="Reasoning effort for the next turn"
                aria-label={`Reasoning effort: ${effortLabel(effort())}`}
                onClick={() => setEffortOpen(!effortOpen())}
              >
                {effortLabel(effort())} <Ic.ChevronDown size={10} />
              </button>
              <Show when={effortOpen()}>
                <div class="menu under-model-menu">
                  <button class={`menu-item ${!effort() ? "on" : ""}`} onClick={() => void pickEffort("")}>
                    <span class="pick-name">{DEFAULT_EFFORT_LABEL}</span>
                    <span class="pick-check">{!effort() ? "✓" : ""}</span>
                  </button>
                  <For each={efforts()}>
                    {(e) => (
                      <button class={`menu-item ${e === effort() ? "on" : ""}`} onClick={() => void pickEffort(e)}>
                        <span class="pick-name">{effortLabel(e)}</span>
                        <span class="pick-check">{e === effort() ? "✓" : ""}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            </Show>
          </div>
        </Show>
      </Show>
      <div class="ctx-wrap">
        <button class="ctx" title="Context used" onClick={() => setOpen(!open())}>
          <Ic.ContextRing pct={pct()} /> {pct()}%
        </button>
        <Show when={open()}>
          <div class="ctx-panel" role="dialog" aria-label="Context">
            <div class="ctx-panel-h">
              <span>Context</span>
              <span class="ctx-panel-meta">{pct()}% · ~{formatTokens(used())} / {formatTokens(windowSize())}</span>
            </div>
            <div class="ctx-bar" aria-hidden="true">
              <i style={{ width: `${pct()}%` }} />
            </div>
            <div class="ctx-row">
              <span>Conversation</span>
              <span>{formatTokens(used())}</span>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}

function MissionView(p: { id: string; onContext?: (id: string, pct: number | null) => void; initial?: Mission; onMission?: (mission: Mission | null) => void; onFork?: (mission: Mission) => void }) {
  const receipt = recalledLaunch(p.id);
  const cached = peekReadyTranscript(p.id);
  const [mission, setMission] = createSignal<Mission | null>(p.initial ?? null);
  createEffect(() => p.onMission?.(mission()));
  onCleanup(() => p.onMission?.(null));
  const [items, setItems] = createSignal<StreamItem[]>(cached?.items ?? []);
  const [awaiting, setAwaiting] = createSignal(!cached);
  const [error, setError] = createSignal<string | null>(null);
  const [queueError, setQueueError] = createSignal<string | null>(cached?.queueError ?? null);
  const [sendError, setSendError] = createSignal<string | null>(null);
  const [followAttach, setFollowAttach] = createSignal<AttachChip[]>([]);
  let scroller: HTMLDivElement | undefined;
  let nearBottom = true;
  cacheRemember(`m:${p.id}`);

  const scrollIfPinned = () => {
    if (nearBottom) scroller?.scrollTo({ top: scroller.scrollHeight });
  };
  // Resize notifications run after streaming Markdown has changed layout.
  onMount(() => {
    if (!scroller) return;
    const observer = new ResizeObserver(() => { if (nearBottom) scrollIfPinned(); });
    const content = scroller.querySelector(".col");
    if (content) observer.observe(content);
    onCleanup(() => observer.disconnect());
  });

  const refresh = async () => {
    try {
      setMission(await getMission(p.id));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // Rebuild the transcript from the stored event log (initial load and
  // resync after stream lag). Stored rows map onto the live event shapes,
  // so the same reducer handles both.
  // Live events that arrive while a replay is in flight would be clobbered
  // by the replay's setItems; hold them and fold them in afterwards.
  let replaying = false;
  let held: StreamEvent[] = [];
  const applyLive = (ev: StreamEvent) => {
    setItems((cur) => {
      const next = applyStreamEvent(cur, ev);
      if (next !== cur) {
        putTranscriptItems(p.id, next);
        queueMicrotask(scrollIfPinned);
      }
      return next;
    });
  };
  const resync = async () => {
    if (replaying) return;
    replaying = true;
    held = [];
    let history: StreamEvent[] = [];
    try {
      const snap = await loadTranscript(p.id);
      history = snap.stream;
      // Absence from a queue snapshot is not proof of delivery: the event
      // logger can lag dequeue. Keep known pending entries until their ID is
      // explicitly delivered, including while a reconnect replay is in flight.
      let next = snap.items;
      for (const item of items()) if (item.kind === "user" && item.queued && item.messageId) {
        next = applyStreamEvent(next, { type: "user_message", data: { id: item.messageId, content: item.text, queued: true, receipt: item.receipt, attached: item.attached } });
      }
      setItems(next);
      putTranscript(p.id, { ...snap, items: next });
      setQueueError(snap.queueError ?? null);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      replaying = false;
      setAwaiting(false);
      const queued = held;
      held = [];
      for (const ev of heldAfterHistory(history, queued)) applyLive(ev);
    }
  };

  onMount(() => {
    void Promise.all([refresh(), resync()]).then(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
    const stopStream = streamMission(
      p.id,
      (ev) => {
        if (ev.type === "mission_status_changed" || ev.type === "status") {
          if (ev.type === "mission_status_changed" && typeof ev.data.status === "string") {
            const status = ev.data.status;
            setMission(old => ({ ...(old ?? {id:p.id,title:null,history:[],created_at:"",updated_at:""}), status,
              status_message: typeof ev.data.summary === "string" ? ev.data.summary : old?.status_message }));
          }
          void refresh();
          return;
        }
        if (replaying) held.push(ev);
        else applyLive(ev);
      },
      () => void resync(),
    );
    // Slow status poll — the stream is authoritative for content, but the
    // composer busy state shouldn't depend on it alone.
    const stopPoll = pollWhileVisible(refresh, 10000);
    onCleanup(() => {
      if (scroller) putTranscriptHeight(p.id, scroller.scrollHeight);
      stopStream();
      stopPoll();
    });
  });

  createEffect(() => {
    const id = p.id;
    if (!localBinding(id)) return;
    const reconcile = () => reconcileLocalRun(id);
    void reconcile();
    const stop = pollWhileVisible(reconcile, 2000);
    onCleanup(stop);
  });

  const clientPlaced = () => !!localBinding(p.id) || !!mission()?.tags?.includes("placement:client");
  const busy = () => {
    if (localRunActive(p.id)) return true;
    const s = mission()?.status;
    return !!s && ["active","running","pending","queued","starting","resuming"].includes(s);
  };

  /** Anything the agent has actually said or done in this turn. */
  const activity = () => viewItems().some((i) => ["text", "tool", "think"].includes(i.kind));
  /**
   * The mission is working and has produced nothing yet: the window where the
   * prompt animates instead of a banner. It stops the moment any output lands —
   * a tool call counts, not just text — and never runs for a state the user
   * has to act on, which keeps its own banner.
   */
  const pending = () => {
    const phase = missionPhase(mission(), activity());
    return (phaseIsQuiet(phase) || phase.label === "Queued") && phase.moving && !activity();
  };
  const phaseLabel = () => missionPhase(mission(), activity()).label;

  const viewItems = () => {
    const list = withInitialPrompt(items(), mission(), receipt);
    const live = localLiveText(p.id);
    const withLive = live && !list.some(item => item.kind === "text" && item.text === live) ? [...list, { kind: "text" as const, key: `local:${p.id}`, text: live, live: localRunActive(p.id) }] : list;
    if (busy()) return withLive;
    // Terminal mission: force-close any bubble left open by a dropped
    // assistant_message finalizer.
    return withLive.map((i) => (i.kind === "text" && i.live ? { ...i, live: false } : i));
  };

  const titleContext = createMemo(() => awaiting() ? null : contextPct(estimateTokens(viewItems()), contextWindow(mission()?.backend)));
  createEffect(() => p.onContext?.(p.id, titleContext()));

  // Retrying an uncertain network result reuses the original message identity.
  // A different draft/selection, or a definitive rejection, starts a new attempt.
  let retryMessage: { key: string; id: string } | null = null;
  const sendMsg = async (text: string, images: DraftImage[] = [], chips: AttachChip[] = followAttach()) => {
    setSendError(null);
    if (clientPlaced()) {
      await import("./localAgents").then(m => m.restoreLocalBindings()).catch(console.error);
      const binding = localBinding(p.id);
      if (!binding) {
        setSendError("This session runs on the computer that started it. Your draft is kept.");
        return false;
      }
      const project = mission()?.project;
      if (!project) {
        setSendError("This mission has no project, so its files cannot be copied. Your draft is kept.");
        return false;
      }
      try {
        const plan = await materializeMentions(project, text, chips);
        if (plan.files.length) await writeLocalFiles(binding.cwd, plan.files);
        const imagePaths = await stageLocalImages(binding.cwd, images);
        const sent = imagePrompt(bindWorkspace(plan.prompt, binding.cwd), imagePaths);
        await startLocal({ id: p.id, harness: binding.harness, bin: binding.bin, cwd: binding.cwd, prompt: sent, model: binding.model, sessionId: binding.sessionId, imagePaths });
        // Persist only accepted turns: a rejected launch must keep the draft
        // without adding another copy to the conversation.
        await appendClientTranscript(p.id, "user", imagePrompt(text, imagePaths)).catch(e => {
          setSendError(`The local run started, but saving your message failed: ${String(e)}`);
        });
        if (chips === followAttach()) setFollowAttach([]);
        void followLocal(p.id, () => {}).then(async (state) => {
          const note = binding.harness === "grok" && binding.sessionId && !state.resumed ? "Grok starts a new local session.\n\n" : "";
          const body = `${note}${state.text}`.trim();
          if (body) await appendClientTranscript(p.id, "assistant", body);
          const failed = (state.exit_code != null && state.exit_code !== 0) || (!!state.error && !state.text.trim());
          if (failed) recordLocalFailure(p.id, state.error || `Local process exited with code ${state.exit_code}`);
          await setClientMissionStatus(p.id, failed ? "failed" : "awaiting_user");
          void refresh();
        }).catch(async (error) => {
          recordLocalFailure(p.id, error);
          await setClientMissionStatus(p.id, "failed").catch(() => {});
          void refresh();
        });
        return true;
      } catch (e) {
        // Launch rejection belongs to the composer; it is not a second mission failure.
        recordLocalFailure(p.id, null);
        setSendError(e instanceof Error ? e.message : String(e));
        return false;
      }
    }
    const attachments = chips.map(chipToAttachment);
    const key = JSON.stringify([connectionVersion(), text, attachments, images.map(image => image.id)]);
    if (retryMessage?.key !== key) retryMessage = { key, id: crypto.randomUUID() };
    try {
      const sent = imagePrompt(text, await stageRemoteImages(images, mission()));
      const result = await sendMissionMessage(p.id, sent, attachments, retryMessage.id);
      retryMessage = null;
      const event: StreamEvent = { type: "user_message", eventId: result.id, data: { id: result.id, content: sent, queued: result.queued, receipt: true, attached: chips.length > 0 } };
      if (replaying) held.push(event);
      else applyLive(event);
      if (chips === followAttach()) setFollowAttach([]);
      void refresh();
      return true;
    }
    catch (e) {
      if (e instanceof MessageRejectedError || (e instanceof ApiError && e.status < 500 && e.status !== 408)) retryMessage = null;
      setSendError(launchError(e)); return false;
    }
  };

  const sendEditedPrompt = async (text: string) => {
    if (clientPlaced() && busy()) throw new Error("Wait for the local agent to finish or stop it before sending this follow-up.");
    // The inline editor sends only this message's existing attachments, not
    // unrelated context chips or an unsent draft in the main composer.
    const accepted = await sendMsg(text, [], []);
    if (!accepted) throw new Error(sendError() || "The message was not sent. Your draft is kept.");
    return true;
  };

  const stopM = () => {
    if (clientPlaced()) {
      void stopLocal(p.id).then(() => setClientMissionStatus(p.id, "interrupted")).then(() => refresh()).catch(e => setSendError(String(e)));
      return;
    }
    void cancelMission(p.id)
      .catch(() => {})
      .then(() => refresh());
  };

  return (
    <>
      <div
        class="scroll"
        ref={scroller}
        onScroll={() => {
          nearBottom = !scroller || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
        }}
      >
        <div class="col" style={peekTranscriptHeight(p.id) && awaiting() ? { "min-height": `${peekTranscriptHeight(p.id)}px` } : undefined}>
          <Show
            when={!awaiting() || !!receipt || items().length > 0}
            fallback={
              <>
                <Show when={receipt}>
                  {(r) => (
                    <>
                      <LaunchStatus destination={missionDestination(mission(), r())} mission={mission()} goal={missionGoal(mission(), r())} />
                      <UserTurn text={r().prompt} pending={pending()} onSend={sendEditedPrompt} />
                      <Show when={pending()}>
                        <MissionPending destination={missionDestination(mission(), r())} label={phaseLabel()} />
                      </Show>
                    </>
                  )}
                </Show>
                <DelayedTranscriptSkeleton />
              </>
            }
          >
            <LaunchStatus submitting={localRunActive(p.id)} destination={missionDestination(mission(), receipt)} mission={mission()} goal={missionGoal(mission(), receipt)} activity={activity()} failureInTranscript={visibleTranscript(viewItems()).some(item => item.kind === "error")} />
            <Transcript items={viewItems().filter(i => i.kind !== "user" || !i.queued)} pending={pending()} onSend={sendEditedPrompt} />
            <Show when={!sendError()}>
              <MissionFailure mission={mission()} active={localRunActive(p.id)} error={localFailure(p.id)} failureInTranscript={visibleTranscript(viewItems()).some(item => item.kind === "error")} />
            </Show>
            <Show when={pending()}>
              <MissionPending destination={missionDestination(mission(), receipt)} label={phaseLabel()} />
            </Show>
          </Show>
        </div>
      </div>
      <div class="dock">
        <div class="col">
          <Show when={items().some(i => i.kind === "user" && i.queued)}>
            <section class="queued-messages" aria-label="Queued messages" aria-live="polite">
              <div class="queued-label">Queued messages</div>
              <ol><For each={items().filter((i): i is Extract<StreamItem, { kind: "user" }> => i.kind === "user" && i.queued === true)}>{item => <li data-message-id={item.messageId}><UserTurn text={item.text} attached={item.attached} /></li>}</For></ol>
            </section>
          </Show>
          <Composer
            placeholder="Send follow-up"
            picker={false}
            busy={busy()}
            onSend={sendMsg}
            onStop={stopM}
            scope={`m:${p.id}`}
            uploadTarget={clientPlaced() ? "local" : mission()?.remote_node_id ?? mission()?.remote_job?.node_id ?? "core"}
            backend={mission()?.backend}
            projectSlug={mission()?.project ?? undefined}
            onAttachments={setFollowAttach}
          />
          <Show when={sendError() || error() || queueError()}>
            <ErrorNotice error={(sendError() || error() || queueError())!} title={sendError() ? "Couldn’t send your message" : "Couldn’t load the conversation"} onDismiss={() => { setSendError(null); setError(null); setQueueError(null); }} />
          </Show>
          <Show when={latestChecklist(items())?.tasks.length}>
            <button class="tasks-jump" onClick={() => { const tasks = scroller?.querySelector<HTMLElement>(".mission-tasks"); tasks?.scrollIntoView({ behavior: "smooth", block: "center" }); tasks?.focus({ preventScroll: true }); }}>Tasks</button>
          </Show>
          <MissionDock mission={mission()} items={viewItems()} destination={missionDestination(mission(), receipt)} onMission={setMission} onError={setError} onFork={p.onFork} />
        </div>
      </div>
    </>
  );
}
