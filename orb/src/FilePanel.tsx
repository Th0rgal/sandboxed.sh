import * as TreeIcon from "./sidebarIcons";
import {
  lazy,
  Suspense,
  createContext,
  useContext,
  createSignal,
  createEffect,
  createMemo,
  on,
  untrack,
  onCleanup,
  onMount,
  Show,
  For,
  type JSX,
} from "solid-js";
import {
  createFileClient,
  fileScopeKey,
  parseFileTarget,
  relativeFilePath,
  type FileScope,
  type FileSource,
  type FileRef,
  type FileEntry,
  type FileRead,
} from "./fileResources";
import {
  FileReferenceContext,
  type ReferenceResolver,
} from "./fileReferenceContext";
import { MdView, ReadOnlySource } from "./Markdown";
import { FileSkeleton } from "./Skeleton";
import { ErrorNotice } from "./ErrorNotice";
import { connectionVersion } from "./api";
import * as Ic from "./icons";

const PdfPreview = lazy(() => import("./PdfPreview"));

type Tab = FileRef & { pinned: boolean; scroll?: number };
type Saved = {
  tabs: Tab[];
  active?: string;
  expanded: string[];
  open: boolean;
};
const identity = (r: FileRef) => `${r.source}:${r.path}`;
const loadSaved = (key: string): Saved => {
  try {
    return (
      JSON.parse(sessionStorage.getItem(`orb.files:${key}`) ?? "null") ?? {
        tabs: [],
        expanded: [],
        open: false,
      }
    );
  } catch {
    return { tabs: [], expanded: [], open: false };
  }
};
const SidePanelContext = createContext<{
  target: () => HTMLDivElement | undefined;
  available: () => boolean;
  visible: () => boolean;
  show: () => void;
  hide: () => void;
  register: (open: (() => void) | undefined) => void;
}>();
export const useSidePanel = () => useContext(SidePanelContext);

const PanelContext = createContext<{
  toggle: () => void;
  open: () => boolean;
  available: () => boolean;
}>();
export function FilePanelButton() {
  const ctx = useContext(PanelContext);
  const side = useSidePanel();
  return (
    <><Show when={ctx?.available()}>
      <button
        class="files-toggle"
        title="Toggle files (⌘J)"
        aria-keyshortcuts="Meta+J"
        aria-label="Files"
        aria-expanded={ctx?.open()}
        onClick={() => ctx?.toggle()}
      >
        <Ic.FileIcon size={16} />
      </button>
    </Show><Show when={side?.available()}><button class="files-toggle" title="Side question" aria-label="Side question" aria-expanded={side?.visible()} onClick={() => side?.visible() ? side.hide() : side?.show()}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M20 11a8 8 0 0 1-8 8H5l-3 3V11a9 9 0 0 1 18 0Z"/><path d="M7 9h8M7 13h5"/></svg></button></Show></>
  );
}
export function FilePanelProvider(p: {
  scope: FileScope;
  children: JSX.Element;
}) {
  const scopeKey = createMemo(() => {
    connectionVersion();
    return fileScopeKey(p.scope);
  });
  const available = () => !!(p.scope.mission?.id || p.scope.project);
  let client = createFileClient(p.scope),
    generation = 0;
  const [sideVisible, setSideVisible] = createSignal(false);
  const [sideTarget, setSideTarget] = createSignal<HTMLDivElement>();
  const [openSide, setOpenSide] = createSignal<(() => void)>();
  const [opened, setFilesOpened] = createSignal(false),
    [sources, setSources] = createSignal<FileSource[]>([]),
    [tabs, setTabs] = createSignal<Tab[]>([]),
    [active, setActive] = createSignal<string>(),
    [expanded, setExpanded] = createSignal<string[]>([]);
  // All file entry points (including links and Cmd+P) select the Files pane.
  const setOpened = (value: boolean | ((previous: boolean) => boolean)) => {
    setSideVisible(false);
    return setFilesOpened(value);
  };
  const [directories, setDirectories] = createSignal<
      Record<string, FileEntry[]>
    >({}),
    [content, setContent] = createSignal<FileRead>(),
    [error, setError] = createSignal<string>(),
    [loading, setLoading] = createSignal(false),
    [placeholder, setPlaceholder] = createSignal(false);
  const [tree, setTree] = createSignal(true),
    [sourceMode, setSourceMode] = createSignal(false),
    [maximized, setMaximized] = createSignal(false),
    [search, setSearch] = createSignal<string | null>(null),
    [results, setResults] = createSignal<FileRef[]>([]),
    [choices, setChoices] = createSignal<FileRef[]>([]),
    [notice, setNotice] = createSignal("");
  const [history, setHistory] = createSignal<FileRef[]>([]),
    [historyIndex, setHistoryIndex] = createSignal(-1);
  let panel: HTMLElement | undefined,
    viewer: HTMLDivElement | undefined,
    searchInput: HTMLInputElement | undefined;
  let cache = new Map<string, Promise<FileRef[]>>(),
    queue = new Map<string, ((r: FileRef[]) => void)[]>(),
    batchTimer: ReturnType<typeof setTimeout> | undefined,
    rootPromise: Promise<FileSource[]> | undefined,
    readGeneration = 0;
  let layoutMarker: HTMLSpanElement | undefined;
  const [layout, setLayout] = createSignal<HTMLElement>();
  onMount(() => {
    const app=layoutMarker?.closest<HTMLElement>(".app");
    setLayout(app ?? undefined);
    const width=Number(localStorage.getItem("orb.files.width"));
    if(app&&width>0)app.style.setProperty("--file-panel-width",`${width}px`);
  });
  createEffect(() => {
    const app = layout();
    if (app) { app.dataset.filesOpen = String((opened() || sideVisible()) && available()); app.dataset.editorExpanded=String((opened() || sideVisible())&&available()&&maximized()); }
  });
  onCleanup(() => {
    const app = layout();
    if (app) { delete app.dataset.filesOpen; delete app.dataset.editorExpanded; }
  });
  const selected = createMemo(() =>
    tabs().find((t) => identity(t) === active()),
  );
  function persist() {
    if (!available()) return;
    try {
      sessionStorage.setItem(
        `orb.files:${scopeKey()}`,
        JSON.stringify({
          tabs: tabs(),
          active: active(),
          expanded: expanded(),
          open: opened(),
        }),
      );
    } catch {}
  }
  async function ensureRoots() {
    if (!rootPromise) {
      const g = generation;
      rootPromise = client
        .roots()
        .then((r) => {
          if (g === generation) {
            setSources(r);
            if (!expanded().length)
              setExpanded(r.filter((s) => s.available).map((s) => s.id + ":"));
          }
          return r;
        })
        .catch((e) => {
          if (g === generation) {
            rootPromise = undefined;
            setError(String(e.message ?? e));
          }
          throw e;
        });
    }
    return rootPromise;
  }
  createEffect(
    on(scopeKey, (key) => {
      generation++;
      readGeneration++;
      client = createFileClient(untrack(() => p.scope));
      rootPromise = undefined;
      cache = new Map();
      if (batchTimer) clearTimeout(batchTimer);
      for (const callbacks of queue.values()) callbacks.forEach((cb) => cb([]));
      queue = new Map();
      const saved = loadSaved(key);
      setTabs(saved.tabs);
      setActive(saved.active);
      setExpanded(saved.expanded);
      setOpened(saved.open);
      setDirectories({});
      setSources([]);
      setContent(undefined);
      setError(undefined);
      setResults([]);
      setChoices([]);
      setSearch(null);
      setHistory([]);
      setHistoryIndex(-1);
    }),
  );
  createEffect(() => {
    opened();
    tabs();
    active();
    expanded();
    persist();
  });
  function rememberScroll() {
    if (viewer && active())
      setTabs((ts) =>
        ts.map((t) =>
          identity(t) === active() ? { ...t, scroll: viewer!.scrollTop } : t,
        ),
      );
  }
  function openFile(ref: FileRef, pinned = false, record = true) {
    rememberScroll();
    const key = identity(ref);
    setOpened(true);
    setSearch(null);
    setChoices([]);
    setTabs((ts) => {
      const existing = ts.find((t) => identity(t) === key);
      return existing
        ? ts.map((t) =>
            identity(t) === key
              ? { ...t, ...ref, pinned: t.pinned || pinned }
              : t,
          )
        : [...ts.filter((t) => t.pinned), { ...ref, pinned }];
    });
    setActive(key);
    setExpanded((xs) => {
      const path = ref.path.split("/");
      const next = new Set(xs);
      next.add(ref.source + ":");
      for (let i = 1; i < path.length; i++)
        next.add(`${ref.source}:${path.slice(0, i).join("/")}`);
      return [...next];
    });
    if (record) {
      const next = [...history().slice(0, historyIndex() + 1), ref];
      setHistory(next);
      setHistoryIndex(next.length - 1);
    }
  }
  async function list(source: string, path: string) {
    const g = generation,
      key = `${source}:${path}`;
    if (directories()[key]) return;
    try {
      await ensureRoots();
      const r = await client.call(source, { action: "list", path });
      if (g === generation) {
        setDirectories((d) => ({ ...d, [key]: r.entries ?? [] }));
        if (r.truncated)
          setNotice("Some entries are omitted; narrow the search.");
      }
    } catch (e) {
      if (g === generation) setError((e as Error).message);
    }
  }
  createEffect(() => {
    if (opened())
      void ensureRoots()
        .then((roots) => {
          for (const root of roots) if (root.available) void list(root.id, "");
        })
        .catch(() => {});
  });
  createEffect(() => {
    for (const key of expanded()) {
      const split = key.indexOf(":");
      if (opened()) void list(key.slice(0, split), key.slice(split + 1));
    }
  });
  const [refreshingFiles,setRefreshingFiles]=createSignal(false);
  async function refreshFiles(){
    if(refreshingFiles())return;
    setRefreshingFiles(true);
    rememberScroll();
    try {
      cache.clear();rootPromise=undefined;setDirectories({});
      const roots=await ensureRoots();
      await Promise.all(roots.filter(root=>root.available).map(root=>list(root.id,"")));
      await Promise.all(expanded().map(key=>{const split=key.indexOf(":");return list(key.slice(0,split),key.slice(split+1));}));
      await readSelected();
    } catch(error){setError(String(error));}
    finally{setRefreshingFiles(false);}
  }
  async function readSelected() {
    const ref = selected();
    if (!ref) return;
    const g = ++readGeneration;
    setLoading(true);
    setPlaceholder(false);
    setError(undefined);
    setContent(undefined);
    const timer = setTimeout(() => {
      if (g === readGeneration) setPlaceholder(true);
    }, 300);
    try {
      await ensureRoots();
      const r = await client.call(ref.source, {
        action: "read",
        path: ref.path,
      });
      if (g !== readGeneration) return;
      setContent(r as FileRead);
      requestAnimationFrame(() => {
        if (g !== readGeneration || !viewer) return;
        if (ref.line) {
          setSourceMode(true);
          requestAnimationFrame(() =>
            viewer
              ?.querySelector(`[data-line="${ref.line}"]`)
              ?.scrollIntoView({ block: "center" }),
          );
        } else viewer.scrollTop = ref.scroll ?? 0;
      });
    } catch (e) {
      if (g === readGeneration) setError((e as Error).message);
    } finally {
      clearTimeout(timer);
      if (g === readGeneration) {
        setLoading(false);
        setPlaceholder(false);
      }
    }
  }
  createEffect(() => {
    active();
    scopeKey();
    if (opened()) void readSelected();
  });
  async function flushReferences() {
    batchTimer = undefined;
    const pending = queue;
    queue = new Map();
    const g = generation;
    const c = client;
    try {
      const roots = await ensureRoots();
      const paths = [...pending.keys()];
      const all = new Map<string, FileRef[]>();
      for (let i = 0; i < paths.length; i += 64) {
        await Promise.all(
          roots
            .filter((s) => s.available)
            .map(async (root) => {
              try {
                const reply = await c.call(root.id, {
                  action: "resolve",
                  paths: paths.slice(i, i + 64),
                });
                for (const r of reply.results ?? [])
                  all.set(r.reference, [
                    ...(all.get(r.reference) ?? []),
                    ...r.matches.map((m) => ({ ...m, source: root.id })),
                  ]);
              } catch {
                /* unavailable sources never manufacture a link */
              }
            }),
        );
      }
      for (const [path, callbacks] of pending)
        callbacks.forEach((cb) =>
          cb(g === generation ? (all.get(path) ?? []) : []),
        );
    } catch {
      for (const callbacks of pending.values())
        callbacks.forEach((cb) => cb([]));
    }
  }
  const resolver: ReferenceResolver = {
    async loadImage(path) {
      const g = generation;
      const c = client;
      const extension = path.split(".").at(-1)?.toLowerCase();
      const mime = ({png:"image/png",jpg:"image/jpeg",jpeg:"image/jpeg",webp:"image/webp",gif:"image/gif"} as Record<string,string>)[extension ?? ""];
      if (!mime) return null;
      const refs = await resolver.resolve(path);
      if (g !== generation || !refs.length) return null;
      const ref = refs[0];
      const chunks: Uint8Array[] = [];
      let offset=0;
      while (true) {
        const part = await c.call(ref.source,{action:"download",path:ref.path,offset});
        if (g !== generation) return null;
        if (!part.bytes?.length || !part.size || part.size > 20 * 1024 * 1024) return null;
        chunks.push(new Uint8Array(part.bytes));
        offset += part.bytes.length;
        if (offset >= part.size) break;
        if (offset > 20 * 1024 * 1024) return null;
      }
      return URL.createObjectURL(new Blob(chunks as BlobPart[],{type:mime}));
    },
    resolve(raw) {
      const parsed = parseFileTarget(raw);
      if (!parsed) return Promise.resolve([]);
      const path = parsed.path;
      if (!cache.has(path))
        cache.set(
          path,
          new Promise((resolve) => {
            queue.set(path, [...(queue.get(path) ?? []), resolve]);
            if (!batchTimer)
              batchTimer = setTimeout(() => void flushReferences(), 80);
          }),
        );
      return cache
        .get(path)!
        .then((refs) => refs.map((r) => ({ ...r, line: parsed.line })));
    },
    open(refs) {
      if (refs.length === 1) openFile(refs[0]);
      else {
        setOpened(true);
        setChoices(refs);
      }
    },
    search(query) {
      setOpened(true);
      setSearch(query);
    },
  };
  createEffect(() => {
    const query = search();
    if (query === null) return;
    const g = generation;
    let cancelled = false;
    const c = client;
    const timer = setTimeout(async () => {
      try {
        const roots = await ensureRoots();
        const found = await Promise.all(
          roots
            .filter((s) => s.available)
            .map(async (r) => {
              const result = await c.call(r.id, { action: "search", query });
              return (result.entries ?? []).map((e) => ({
                ...e,
                source: r.id,
              }));
            }),
        );
        if (!cancelled && g === generation) setResults(found.flat());
      } catch (e) {
        if (!cancelled && g === generation) setError((e as Error).message);
      }
    }, 200);
    onCleanup(() => {
      cancelled = true;
      clearTimeout(timer);
    });
  });
  // Remember the most recently interacted pane, including clicks on read-only
  // source text and toolbar buttons that Safari does not focus on click.
  let filePaneFocused=false;
  const trackPane=(event:Event)=>{filePaneFocused=!!panel?.contains(event.target as Node);};
  onMount(()=>{window.addEventListener("pointerdown",trackPane,true);window.addEventListener("focusin",trackPane,true);});
  onCleanup(()=>{window.removeEventListener("pointerdown",trackPane,true);window.removeEventListener("focusin",trackPane,true);});
  function keys(e: KeyboardEvent) {
    if (!available()) return;
    if (opened() && filePaneFocused && e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey && e.key.toLowerCase()==="b" && !e.isComposing) {
      e.preventDefault();e.stopImmediatePropagation();
      if(!e.repeat)setTree(v=>!v);
      return;
    }
    if ((opened() || sideVisible()) && e.metaKey && !e.ctrlKey && !e.altKey && e.shiftKey && e.key.toLowerCase() === "f" && !e.isComposing) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!e.repeat) setMaximized(v => !v);
      return;
    }
    if (e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "j" && !e.isComposing) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!e.repeat) setOpened(v => !v);
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "p") {
      e.preventDefault();
      e.stopImmediatePropagation();
      setOpened(true);
      setSearch("");
      requestAnimationFrame(() => searchInput?.focus());
    }
    if (e.metaKey && e.key === "/" && panel?.contains(document.activeElement)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      setSourceMode((v) => !v);
    }
    if (e.key === "Escape" && document.querySelector(".find-bar")) return;
    if (e.key === "Escape" && (maximized() || panel?.contains(document.activeElement))) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (search() !== null) setSearch(null);
      else if (maximized()) setMaximized(false);
      else setOpened(false);
    }
  }
  const closeSideOnEscape = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !event.defaultPrevented && sideVisible()) {
      event.preventDefault();setSideVisible(false);
    }
  };
  onMount(() => window.addEventListener("keydown", closeSideOnEscape));
  onCleanup(() => window.removeEventListener("keydown", closeSideOnEscape));
  onMount(() => window.addEventListener("keydown", keys, true));
  onCleanup(() => {
    window.removeEventListener("keydown", keys, true);
    if (batchTimer) clearTimeout(batchTimer);
  });
  function resize(e: PointerEvent, inside = false) {
    e.preventDefault();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const start = e.clientX;
    const activePanel = target.closest<HTMLElement>(".file-panel") ?? panel;
    const initial = inside
      ? (activePanel?.querySelector(".file-tree")?.getBoundingClientRect().width ??
        220)
      : (activePanel?.getBoundingClientRect().width ?? 480);
    const app = activePanel?.closest<HTMLElement>(".app");
    const conversation = !inside ? app?.querySelector<HTMLElement>(".scroll") : undefined;
    const viewportTop = conversation?.getBoundingClientRect().top ?? 0;
    const anchor = conversation ? Array.from(conversation.querySelectorAll<HTMLElement>(".user, .agent-turn p, .agent-turn li, .agent-turn pre, .md-view p, .md-view pre"))
      .find(el => el.getBoundingClientRect().bottom > viewportTop + 1) : undefined;
    const anchorTop = anchor?.getBoundingClientRect().top;
    const initialScroll = conversation?.scrollTop ?? 0;
    const restoreAnchor = () => {
      if (!conversation) return;
      if (anchor?.isConnected && anchorTop !== undefined) conversation.scrollTop += anchor.getBoundingClientRect().top - anchorTop;
      else conversation.scrollTop = initialScroll;
    };
    if (conversation) conversation.dataset.panelResizing = "true";
    const sidebarWidth = app ? parseFloat(getComputedStyle(app).gridTemplateColumns) || 0 : 0;
    const maximum = Math.max(480, (app?.clientWidth ?? window.innerWidth) - sidebarWidth - 420);
    const move = (event: PointerEvent) => {
      const value = inside
        ? Math.max(150, Math.min(400, initial + event.clientX - start))
        : Math.max(
            480,
            Math.min(maximum, initial + start - event.clientX),
          );
      (inside ? activePanel : app)?.style.setProperty(
        inside ? "--file-tree-width" : "--file-panel-width",
        `${value}px`,
      );
      restoreAnchor();
      try {
        localStorage.setItem(
          inside ? "orb.files.treeWidth" : "orb.files.width",
          String(value),
        );
      } catch {}
    };
    const end = () => {
      requestAnimationFrame(() => requestAnimationFrame(() => {
        restoreAnchor();
        if (conversation) delete conversation.dataset.panelResizing;
      }));
      target.removeEventListener("lostpointercapture", end);
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", end);
      target.removeEventListener("pointercancel", end);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", end);
    target.addEventListener("pointercancel", end);
    target.addEventListener("lostpointercapture", end);
  }
  function Rows(q: { source: string; path: string; depth: number }) {
    const key = () => `${q.source}:${q.path}`;
    return (
      <For each={directories()[key()] ?? []}>
        {(entry) => {
          const ref = { ...entry, source: q.source };
          const dir = entry.kind === "dir",
            expandedKey = identity(ref);
          return (
            <div class="file-tree-node">
              <button
                role="treeitem"
                aria-level={q.depth + 1}
                aria-expanded={
                  dir ? expanded().includes(expandedKey) : undefined
                }
                aria-selected={active() === identity(ref)}
                class={`file-tree-row ${active() === identity(ref) ? "selected" : ""}`}

                onClick={() =>
                  dir
                    ? setExpanded((xs) =>
                        xs.includes(expandedKey)
                          ? xs.filter((x) => x !== expandedKey)
                          : [...xs, expandedKey],
                      )
                    : openFile(ref)
                }
                onDblClick={() => !dir && openFile(ref, true)}
              >
                <span>
                  {dir ? (
                    expanded().includes(expandedKey) ? (
                      <Ic.ChevronDown size={12} />
                    ) : (
                      <Ic.ChevronRight size={12} />
                    )
                  ) : (
                    <Ic.FileIcon size={14} />
                  )}
                </span>
                <Show when={dir}><span class="file-folder-icon">{expanded().includes(expandedKey)?<TreeIcon.FolderOpen size={14}/>:<TreeIcon.Folder size={14}/>}</span></Show>
                <span class="file-entry-name">{entry.name}</span>
              </button>
              <Show when={dir && expanded().includes(expandedKey)}>
                <div class="file-tree-children" role="group"><Rows source={q.source} path={entry.path} depth={q.depth + 1} /></div>
              </Show>
            </div>
          );
        }}
      </For>
    );
  }
  const [showPdf, setShowPdf] = createSignal(false);
  createEffect(on(() => [active(), scopeKey(), content()], () => setShowPdf(false)));
  const isLocalFile = () => sources().some(s => s.id === selected()?.source && s.local);
  async function loadPdf(signal: AbortSignal): Promise<Uint8Array> {
    const ref = selected();
    if (!ref) throw new Error("No file selected");
    const c = client;
    const chunks: Uint8Array[] = [];
    let offset = 0, total = 1;
    while (offset < total) {
      signal.throwIfAborted();
      const part = await c.call(ref.source, {action:"download",path:ref.path,offset});
      signal.throwIfAborted();
      if (part.size === undefined || part.size > 50 * 1024 * 1024) throw new Error("PDF preview is limited to 50 MB. Open the full file externally.");
      if (!part.bytes?.length || part.next !== offset + part.bytes.length) throw new Error("Incomplete PDF download");
      chunks.push(new Uint8Array(part.bytes));
      offset = part.next; total = part.size;
    }
    const bytes = new Uint8Array(offset);
    let position = 0;
    for (const chunk of chunks) { bytes.set(chunk, position); position += chunk.length; }
    return bytes;
  }
  async function revealFile() {
    const ref = selected();
    if (!ref) return;
    setError(undefined);
    try {
      await client.call(ref.source, { action: "reveal", path: ref.path });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }
  async function download() {
    const ref = selected();
    if (!ref) return;
    setError(undefined);
    try {
      const chunks: Uint8Array[] = [];
      let offset = 0,
        total = 1;
      while (offset < total) {
        const r = await client.call(ref.source, {
          action: "download",
          path: ref.path,
          offset,
        });
        if (!r.bytes?.length) break;
        chunks.push(new Uint8Array(r.bytes));
        offset = r.next!;
        total = r.size!;
      }
      const url = URL.createObjectURL(new Blob(chunks as BlobPart[]));
      const a = document.createElement("a");
      a.href = url;
      a.download = ref.name;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      setError((e as Error).message);
    }
  }
  const closeTab = (key: string) => {
    rememberScroll();
    setTabs((ts) => ts.filter((t) => identity(t) !== key));
    if (active() === key)
      setActive(tabs().at(-1) ? identity(tabs().at(-1)!) : undefined);
  };
  return (
    <SidePanelContext.Provider value={{
      target: sideTarget,
      available: () => !!openSide(),
      visible: sideVisible,
      show: () => {
        if (!openSide()) return;
        setFilesOpened(false);
        setSideVisible(true);
        openSide()?.();
      },
      hide: () => setSideVisible(false),
      register: fn => {
        setOpenSide(() => fn);
        if (!fn) setSideVisible(false);
      },
    }}>
    <PanelContext.Provider
      value={{ toggle: () => setOpened((v) => !v), open: opened, available }}
    >
      <FileReferenceContext.Provider value={resolver}>
        <span ref={layoutMarker} hidden />
        {p.children}
        <aside class="btw-sidebar" classList={{ "file-panel": sideVisible(), maximized: sideVisible() && maximized() }} style={{ display: sideVisible() ? undefined : "none" }} aria-label="Side question panel"><div class="file-panel-resize" role="separator" aria-label="Resize side question panel" aria-orientation="vertical" onPointerDown={e=>resize(e)} /><div ref={setSideTarget} class="btw-sidebar-content" /></aside>
        <Show when={opened() && available()}>
          <aside
            ref={(el) => {
              panel = el;
              el.style.setProperty(
                "--file-tree-width",
                `${Number(localStorage.getItem("orb.files.treeWidth")) || 220}px`,
              );
              requestAnimationFrame(() =>
                el
                  .closest<HTMLElement>(".app")
                  ?.style.setProperty(
                    "--file-panel-width",
                    `${Number(localStorage.getItem("orb.files.width")) || Math.round((window.innerWidth - 220) * 0.45)}px`,
                  ),
              );
            }}
            class={`file-panel ${maximized() ? "maximized" : ""}`}
            aria-label="Files"
            tabIndex={-1}
          >
            <div
              class="file-panel-resize"
              role="separator"
              aria-label="Resize file panel"
              onPointerDown={(e) => resize(e)}
            />
            <div class="file-tabs file-header" data-tauri-drag-region>
              <button aria-label={maximized()?"Restore conversation":"Expand file editor"} title={maximized()?"Restore conversation (⌘⇧F)":"Expand file editor (⌘⇧F)"} aria-keyshortcuts="Meta+Shift+F" aria-pressed={maximized()} onClick={()=>setMaximized(v=>!v)}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d={maximized()?"M4 9h5V4m6 0v5h5M4 15h5v5m6 0v-5h5":"M9 4H4v5m11-5h5v5M4 15v5h5m11-5v5h-5"}/></svg>
              </button>
              <button
                aria-label="Toggle file explorer"
                title="Toggle file explorer (⌘B)"
                aria-keyshortcuts="Meta+B"
                aria-expanded={tree()}
                onClick={() => setTree((v) => !v)}
              >
                <Ic.SidebarIcon size={16} />
              </button>
              <button
                aria-label="Find file"
                onClick={() => {
                  setSearch("");
                  requestAnimationFrame(() => searchInput?.focus());
                }}
              >
                <Ic.SearchIcon size={16} />
              </button>
              <div class="file-tab-list">
                <Show when={!tabs().length}>
                  <span class="file-header-title">Files</span>
                </Show>
                <For each={tabs()}>
                  {(tab) => (
                    <div
                      class={`file-tab ${active() === identity(tab) ? "selected" : ""}`}
                    >
                      <button
                        style={{
                          "font-style": tab.pinned ? "normal" : "italic",
                        }}
                        title={tab.path}
                        onClick={() => openFile(tab, tab.pinned)}
                        onDblClick={() => openFile(tab, true)}
                      >
                        <Ic.FileIcon size={14} />
                        <span>{tab.name}</span>
                      </button>
                      <button
                        aria-label={`Close ${tab.name}`}
                        onClick={() => closeTab(identity(tab))}
                      >
                        <Ic.CloseIcon size={14} />
                      </button>
                    </div>
                  )}
                </For>
              </div>
              <Show when={selected()?.name.match(/\.mdx?$/i)}>
                <button
                  class="file-mode"
                  title="Toggle Markdown/source (⌘/)"
                  onClick={() => setSourceMode((v) => !v)}
                >
                  {sourceMode() ? "Preview" : "Markdown"}
                </button>
              </Show>
              <button aria-label="Reload files" title="Reload files" disabled={refreshingFiles()} onClick={()=>void refreshFiles()}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 7v5h-5M4 17v-5h5"/><path d="M6.1 6.1A8 8 0 0 1 20 12M4 12a8 8 0 0 0 13.9 5.9"/></svg>
              </button>
              <button aria-label="Close files" onClick={() => setOpened(false)}>
                <Ic.CloseIcon size={14} />
              </button>
            </div>
            <Show when={search() !== null || choices().length}>
              <div class="file-search" role="search" aria-label="Search files">
                <button class="file-search-close" aria-label="Close file search" onClick={()=>{setSearch(null);setChoices([]);}}><Ic.CloseIcon size={14}/></button>
                <Show when={search() !== null}>
                  <input
                    ref={searchInput}
                    aria-label="Find file by name or path"
                    placeholder="Find file by name or path…"
                    value={search() ?? ""}
                    onInput={(e) => setSearch(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && results().length)
                        openFile(results()[0]);
                    }}
                  />
                </Show>
                <div class="file-search-results">
                  <For each={choices().length ? choices() : results()}>
                    {(ref) => (
                      <button onClick={() => openFile(ref)}>
                        {ref.path}
                        <small>
                          {sources().find((s) => s.id === ref.source)?.label}
                        </small>
                      </button>
                    )}
                  </For>
                  <Show when={search() !== null && !results().length}>
                    <p>{search()?.trim()?"No results found":"Type a file name or path"}</p>
                  </Show>
                </div>
              </div>
            </Show>
            <div class="file-body">
              <Show when={tree()}>
                <nav class="file-tree" aria-label="File explorer">
                  <For each={sources()}>
                    {(root) => (
                      <section class="file-root">
                        <button
                          class="file-source"
                          title={[root.label, root.machine, root.path]
                            .filter(Boolean)
                            .join(" · ")}
                          aria-expanded={expanded().includes(root.id + ":")}
                          onClick={() =>
                            setExpanded((xs) =>
                              xs.includes(root.id + ":")
                                ? xs.filter((x) => x !== root.id + ":")
                                : [...xs, root.id + ":"],
                            )
                          }
                        >
                          {expanded().includes(root.id + ":") ? (
                            <Ic.ChevronDown size={12} />
                          ) : (
                            <Ic.ChevronRight size={12} />
                          )}
                          <span class="file-root-icon">{root.id==='controller'?<TreeIcon.Clock size={16}/>:root.id==='context'?<TreeIcon.FolderOpen size={16}/>:root.local?<Ic.LaptopIcon size={16}/>:<TreeIcon.Server size={16}/>}</span>
                          <span class="file-root-text"><span class="file-source-label">{root.label.split(" · ")[0]}</span>
                          </span>
                        </button>
                        <Show when={!root.available}>
                          <p class="file-muted">Source unavailable</p>
                        </Show>
                        <Show
                          when={
                            root.available && expanded().includes(root.id + ":")
                          }
                        >
                          <div role="tree" class="file-root-children" aria-label={root.label.split(" · ")[0]}>
                            <Rows source={root.id} path="" depth={0} />
                          </div>
                        </Show>
                      </section>
                    )}
                  </For>
                  <Show when={!sources().length}>
                    <p class="file-muted">No file source is available.</p>
                  </Show>
                  <div
                    class="file-tree-resize"
                    role="separator"
                    aria-label="Resize file explorer"
                    onPointerDown={(e) => resize(e, true)}
                  />
                </nav>
              </Show>
              <div
                class="file-preview"
                classList={{ "file-preview-binary": !loading() && !!content()?.binary }}
                ref={viewer}
                onScroll={() => {
                  const t = selected();
                  if (t && viewer) t.scroll = viewer.scrollTop;
                }}
              >
                <Show when={error()}>
                  <ErrorNotice error={error()!} title="Couldn’t open files">
                    <button
                      onClick={() => {
                        rootPromise = undefined;
                        void ensureRoots().catch(() => {});
                        void readSelected();
                      }}
                    >
                      Retry
                    </button>
                  </ErrorNotice>
                </Show>
                <Show when={placeholder()}>
                  <FileSkeleton />
                </Show>
                <Show when={!selected() && !error()}>
                  <div class="file-empty">
                    <button
                      onClick={() => {
                        setSearch("");
                        requestAnimationFrame(() => searchInput?.focus());
                      }}
                    >
                      Open File
                    </button>
                    <small>Read-only · ⌘P</small>
                  </div>
                </Show>
                <Show when={!loading() && content()}>
                  {(data) => (
                    <>
                      <Show when={!data().binary}><div class="file-provenance">
                        {
                          sources().find((s) => s.id === selected()?.source)
                            ?.label
                        }{" "}
                        · Read-only · {data().size.toLocaleString()} bytes
                        {data().modified
                          ? ` · Modified ${new Date(data().modified! * 1000).toLocaleString()}`
                          : ""}
                      </div></Show>
                      <Show when={data().truncated && !data().binary}>
                        <p class="file-muted">
                          Preview limited to 1 MiB. {isLocalFile() ? "Reveal in Finder to access the full file." : "Download for the full file."}
                        </p>
                      </Show>
                      <Show
                        when={!data().binary}
                        fallback={
                          <Show when={showPdf()} fallback={<div class="file-binary-state">
                            <div class="file-document-mark" aria-hidden="true"><Ic.FileIcon size={40} /><span>{selected()?.name.split(".").pop()?.slice(0, 8).toUpperCase() || "FILE"}</span></div>
                            <h3>{selected()?.name}</h3>
                            <div class="file-binary-details">{data().size < 1024 ? `${data().size} B` : data().size < 1048576 ? `${Math.round(data().size / 1024)} KB` : `${(data().size / 1048576).toFixed(1)} MB`}<span>·</span>{sources().find(s => s.id === selected()?.source)?.label}</div>
                            <p>{/\.pdf$/i.test(selected()?.name ?? "") ? "View this document directly in Orb." : "Preview isn’t available for this file."}</p>
                            <Show when={/\.pdf$/i.test(selected()?.name ?? "")}><button class="s-btn file-binary-action" onClick={() => setShowPdf(true)}><Ic.FileIcon size={16} />View PDF</button></Show>
                            <button class="s-btn file-binary-action" onClick={() => void (isLocalFile() ? revealFile() : download())}>
                              <Ic.FolderOpenIcon size={16} />{isLocalFile() ? "Reveal in Finder" : "Download file"}
                            </button>
                            <Show when={data().modified}><small>Modified {new Date(data().modified! * 1000).toLocaleDateString(undefined, {day:"numeric", month:"short", year:"numeric"})}</small></Show>
                          </div>}>
                            <Suspense fallback={<p class="file-muted">Loading PDF viewer…</p>}><PdfPreview name={selected()?.name ?? "PDF"} load={loadPdf} close={() => setShowPdf(false)} /></Suspense>
                          </Show>
                        }
                      >
                        <FileReferenceContext.Provider
                          value={{
                            ...resolver,
                            resolve: (raw) => {
                              const target = parseFileTarget(raw),
                                ref = selected();
                              if (!target || !ref) return Promise.resolve([]);
                              const path = relativeFilePath(
                                ref.path,
                                target.path,
                              );
                              if (path === null) return Promise.resolve([]);
                              return client
                                .call(ref.source, {
                                  action: "resolve",
                                  paths: [path],
                                })
                                .then((r) =>
                                  (r.results?.[0]?.matches ?? []).map((m) => ({
                                    ...m,
                                    source: ref.source,
                                    line: target.line,
                                  })),
                                );
                            },
                          }}
                        >
                          <Show
                            when={
                              selected()?.name.match(/\.mdx?$/i) &&
                              !sourceMode()
                            }
                            fallback={
                              <ReadOnlySource
                                text={data().content ?? ""}
                                language={selected()?.name.endsWith(".lean")?"lean":undefined}
                                line={selected()?.line}
                              />
                            }
                          >
                            <MdView text={data().content ?? ""} />
                          </Show>
                        </FileReferenceContext.Provider>
                      </Show>
                    </>
                  )}
                </Show>
              </div>
            </div>
            <Show when={notice()}>
              <div class="file-muted">{notice()}</div>
            </Show>
          </aside>
        </Show>
      </FileReferenceContext.Provider>
    </PanelContext.Provider>
    </SidePanelContext.Provider>
  );
}
