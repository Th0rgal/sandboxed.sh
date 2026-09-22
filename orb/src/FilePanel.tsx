import {
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
const PanelContext = createContext<{
  toggle: () => void;
  open: () => boolean;
  available: () => boolean;
}>();
export function FilePanelButton() {
  const ctx = useContext(PanelContext);
  return (
    <Show when={ctx?.available()}>
      <button
        class="files-toggle"
        title="Files (⌘P to find a file)"
        aria-label="Files"
        aria-expanded={ctx?.open()}
        onClick={() => ctx?.toggle()}
      >
        <Ic.FileIcon size={16} />
      </button>
    </Show>
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
  const [opened, setOpened] = createSignal(false),
    [sources, setSources] = createSignal<FileSource[]>([]),
    [tabs, setTabs] = createSignal<Tab[]>([]),
    [active, setActive] = createSignal<string>(),
    [expanded, setExpanded] = createSignal<string[]>([]);
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
  function keys(e: KeyboardEvent) {
    if (!available()) return;
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
    if (e.key === "Escape" && panel?.contains(document.activeElement)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (search() !== null) setSearch(null);
      else setOpened(false);
    }
  }
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
    const initial = inside
      ? (panel?.querySelector(".file-tree")?.getBoundingClientRect().width ??
        220)
      : (panel?.getBoundingClientRect().width ?? 480);
    const app = panel?.closest<HTMLElement>(".app");
    const move = (event: PointerEvent) => {
      const value = inside
        ? Math.max(150, Math.min(400, initial + event.clientX - start))
        : Math.max(
            480,
            Math.min(window.innerWidth - 440, initial + start - event.clientX),
          );
      (inside ? panel : app)?.style.setProperty(
        inside ? "--file-tree-width" : "--file-panel-width",
        `${value}px`,
      );
      try {
        localStorage.setItem(
          inside ? "orb.files.treeWidth" : "orb.files.width",
          String(value),
        );
      } catch {}
    };
    const end = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", end);
      target.removeEventListener("pointercancel", end);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", end);
    target.addEventListener("pointercancel", end);
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
            <>
              <button
                role="treeitem"
                aria-level={q.depth + 1}
                aria-expanded={
                  dir ? expanded().includes(expandedKey) : undefined
                }
                aria-selected={active() === identity(ref)}
                class={`file-tree-row ${active() === identity(ref) ? "selected" : ""}`}
                style={{ "padding-left": `${10 + q.depth * 12}px` }}
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
                  {dir ? (expanded().includes(expandedKey) ? "⌄" : "›") : "≡"}
                </span>
                {entry.name}
              </button>
              <Show when={dir && expanded().includes(expandedKey)}>
                <Rows source={q.source} path={entry.path} depth={q.depth + 1} />
              </Show>
            </>
          );
        }}
      </For>
    );
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
    <PanelContext.Provider
      value={{ toggle: () => setOpened((v) => !v), open: opened, available }}
    >
      <FileReferenceContext.Provider value={resolver}>
        {p.children}
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
            <div class="file-tabs">
              <For each={tabs()}>
                {(tab) => (
                  <div
                    class={`file-tab ${active() === identity(tab) ? "selected" : ""}`}
                  >
                    <button
                      style={{ "font-style": tab.pinned ? "normal" : "italic" }}
                      title={tab.path}
                      onClick={() => openFile(tab, tab.pinned)}
                      onDblClick={() => openFile(tab, true)}
                    >
                      {tab.name}
                    </button>
                    <button
                      aria-label={`Close ${tab.name}`}
                      onClick={() => closeTab(identity(tab))}
                    >
                      ×
                    </button>
                  </div>
                )}
              </For>
              <span class="file-toolbar-spacer" />
              <button
                title="Expand files"
                onClick={() => setMaximized((v) => !v)}
              >
                ⤢
              </button>
              <button aria-label="Close files" onClick={() => setOpened(false)}>
                ×
              </button>
            </div>
            <div class="file-toolbar">
              <button
                aria-label="Toggle file explorer"
                onClick={() => setTree((v) => !v)}
              >
                ☷
              </button>
              <button
                aria-label="Find file"
                onClick={() => {
                  setSearch("");
                  requestAnimationFrame(() => searchInput?.focus());
                }}
              >
                ⌕
              </button>
              <button
                aria-label="Previous file"
                disabled={historyIndex() <= 0}
                onClick={() => {
                  setHistoryIndex((i) => i - 1);
                  openFile(history()[historyIndex()], false, false);
                }}
              >
                ←
              </button>
              <button
                aria-label="Next file"
                disabled={historyIndex() >= history().length - 1}
                onClick={() => {
                  setHistoryIndex((i) => i + 1);
                  openFile(history()[historyIndex()], false, false);
                }}
              >
                →
              </button>
              <span class="file-breadcrumb" title={selected()?.path}>
                {selected()?.path ?? "Files"}
              </span>
              <Show when={selected()?.name.match(/\.mdx?$/i)}>
                <button
                  class={!sourceMode() ? "on" : ""}
                  onClick={() => setSourceMode(false)}
                >
                  Preview
                </button>
                <button
                  class={sourceMode() ? "on" : ""}
                  onClick={() => setSourceMode(true)}
                >
                  Markdown
                </button>
              </Show>
              <details class="file-actions">
                <summary aria-label="File actions">···</summary>
                <div>
                  <button
                    disabled={!selected()}
                    onClick={() =>
                      void navigator.clipboard.writeText(selected()!.path)
                    }
                  >
                    Copy path
                  </button>
                  <button
                    onClick={() => {
                      setDirectories({});
                      cache.clear();
                      rootPromise = undefined;
                      void ensureRoots()
                        .then((roots) => {
                          for (const r of roots)
                            if (r.available) void list(r.id, "");
                        })
                        .catch(() => {});
                      void readSelected();
                    }}
                  >
                    Refresh
                  </button>
                  <button
                    disabled={!selected()}
                    onClick={() => void download()}
                  >
                    Download
                  </button>
                </div>
              </details>
            </div>
            <Show when={search() !== null || choices().length}>
              <div class="file-search">
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
                    <p>No matching files</p>
                  </Show>
                </div>
              </div>
            </Show>
            <div class="file-body">
              <Show when={tree()}>
                <nav class="file-tree" aria-label="File explorer">
                  <For each={sources()}>
                    {(root) => (
                      <section>
                        <button
                          class="file-source"
                          title={root.path}
                          onClick={() =>
                            setExpanded((xs) =>
                              xs.includes(root.id + ":")
                                ? xs.filter((x) => x !== root.id + ":")
                                : [...xs, root.id + ":"],
                            )
                          }
                        >
                          {root.label}
                          {root.machine ? ` · ${root.machine}` : ""}
                        </button>
                        <Show when={!root.available}>
                          <p class="file-muted">Source unavailable</p>
                        </Show>
                        <Show
                          when={
                            root.available && expanded().includes(root.id + ":")
                          }
                        >
                          <div role="tree">
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
                      <div class="file-provenance">
                        {
                          sources().find((s) => s.id === selected()?.source)
                            ?.label
                        }{" "}
                        · Read-only · {data().size.toLocaleString()} bytes
                        {data().modified
                          ? ` · Modified ${new Date(data().modified! * 1000).toLocaleString()}`
                          : ""}
                      </div>
                      <Show when={data().truncated}>
                        <p class="file-muted">
                          Preview limited to 1 MiB. Download for the full file.
                        </p>
                      </Show>
                      <Show
                        when={!data().binary}
                        fallback={
                          <p class="file-muted">
                            Binary file. Use Download to open it.
                          </p>
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
  );
}
