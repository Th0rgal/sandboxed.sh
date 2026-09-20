import { For, Show, createSignal, onCleanup, onMount, createEffect, on } from "solid-js";
import { mergeById, pollWhileVisible } from "./poll";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { MdSource, MdView } from "./Markdown";
import { displayTitle } from "./goal";
import { nodeLabel } from "./missionLaunch";
import {
  isConnected,
  ApiError,
  connectionVersion,
  listProjectFiles,
  listProjectMissions,
  listProjects,
  createProjectCron,
  listProjectCrons,
  getProjectCronDefaults,
  mkdirProjectFile,
  readProjectFile,
  writeProjectFile,
  type Mission,
  type ProjectFileEntry,
  type ProjectSummary,
  projectsVersion,
  getProjectController,
  type ControllerView as ControllerData,
} from "./api";
import { CronGlyph, untilLabel } from "./Controller";
import { Dialog, Field } from "./Dialog";
import { PopupMenu, type MenuEntry } from "./Menu";
import { CronForm } from "./ControllerSettings";
import { getProjectCronFromJob } from "./cronSchema";
import { loadTranscript, prefetchTranscript } from "./missionCache";
import { cacheLoad, cachePeek, cachePrefetch, cachePut, cacheRemember } from "./pageCache";
import { FileSkeleton } from "./Skeleton";

/** Sidebar section listing the core backend's projects with their missions
 * and hosted files. Replaces the demo projects when connected. */
/** Known placement only: remote node, then workspace. Never invented. */
export function missionMachine(m: { remote_job?: { node_id?: string } | null; remote_node_id?: string | null; workspace_name?: string | null }): string | undefined {
  const id = m.remote_job?.node_id ?? m.remote_node_id ?? m.workspace_name;
  return id ? nodeLabel(id) : undefined;
}

export type RowTipContent = { title: string; meta: string[] };
const ROW_TIP_ID = "orb-row-tip";

/** Full title plus known repo/branch/machine lines. Never invented. */
export function rowDetail(title: string, extra: Array<string | undefined | null> = []): RowTipContent {
  return { title, meta: extra.map((part) => part?.trim()).filter((part): part is string => !!part) };
}

/** Prefer the right of the row; otherwise below. Clamp to the viewport. */
export function placeRowTip(
  row: { top: number; left: number; right: number; bottom: number },
  size: { width: number; height: number },
  view: { width: number; height: number },
  gap = 8,
) {
  const pad = 8;
  const beside = view.width - row.right - gap - pad >= Math.min(size.width, 120);
  const x = beside ? row.right + gap : row.left;
  const y = beside ? row.top : row.bottom + gap;
  return {
    x: Math.max(pad, Math.min(x, view.width - size.width - pad)),
    y: Math.max(pad, Math.min(y, view.height - size.height - pad)),
  };
}

/** Where an agent runs: the workspace/machine name behind a cloud glyph.
 * Per agent, not per project — one project can run on several machines. */
function MachineBadge(p: { name?: string | null }) {
  return (
    <Show when={p.name}>
      <span class="row-machine" aria-hidden="true">
        <Ic.CloudIcon />
      </span>
    </Show>
  );
}

function useRowTip() {
  const [tip, setTip] = createSignal<{ title: string; meta: string[]; x: number; y: number } | null>(null);
  let timer = 0;
  let gen = 0;
  let owner: HTMLElement | null = null;
  let card: HTMLDivElement | undefined;
  const unlink = () => { owner?.removeAttribute("aria-describedby"); owner = null; };
  const hide = () => { window.clearTimeout(timer); timer = 0; gen++; unlink(); setTip(null); };
  const place = (el: HTMLElement, content: RowTipContent, size = { width: 240, height: 44 }) => {
    const pos = placeRowTip(el.getBoundingClientRect(), size, { width: window.innerWidth, height: window.innerHeight });
    setTip({ ...content, ...pos });
    requestAnimationFrame(() => {
      if (owner !== el || !card || card.hidden) return;
      const next = placeRowTip(el.getBoundingClientRect(), { width: card.offsetWidth, height: card.offsetHeight }, { width: window.innerWidth, height: window.innerHeight });
      setTip((cur) => cur && owner === el && (cur.x !== next.x || cur.y !== next.y) ? { ...cur, ...next } : cur);
    });
  };
  const show = (content: RowTipContent, el: HTMLElement) => {
    window.clearTimeout(timer);
    const id = ++gen;
    timer = window.setTimeout(() => {
      if (id !== gen || !el.isConnected) return;
      timer = 0;
      unlink();
      owner = el;
      el.setAttribute("aria-describedby", ROW_TIP_ID);
      place(el, content);
    }, 480);
  };
  const bind = (content: RowTipContent) => ({
    onPointerEnter: (e: { currentTarget: HTMLElement }) => show(content, e.currentTarget),
    onPointerLeave: hide,
    onPointerDown: hide,
    onFocus: (e: { currentTarget: HTMLElement }) => {
      if (!e.currentTarget.matches(":focus-visible")) return;
      show(content, e.currentTarget);
    },
    onBlur: hide,
  });
  onMount(() => {
    const dismiss = (e: Event) => {
      if (!timer && !tip()) return;
      if (e.type === "keydown") {
        if ((e as KeyboardEvent).key !== "Escape") return;
        e.preventDefault();
        e.stopPropagation();
      }
      hide();
    };
    window.addEventListener("scroll", dismiss, true);
    window.addEventListener("keydown", dismiss, true);
    window.addEventListener("pointerdown", dismiss, true);
    window.addEventListener("resize", dismiss);
    onCleanup(() => {
      window.removeEventListener("scroll", dismiss, true);
      window.removeEventListener("keydown", dismiss, true);
      window.removeEventListener("pointerdown", dismiss, true);
      window.removeEventListener("resize", dismiss);
    });
  });
  onCleanup(hide);
  return { tip, bind, hide, id: ROW_TIP_ID, setCard: (el: HTMLDivElement) => { card = el; } };
}

export function LiveProjectsSection(p: {
  selected: () => string | null;
  open: (id: string) => void;
  missionGlyph: (status: string) => "idle" | "running" | "pr-closed" | "pr-merged";
  StatusGlyph: (props: { agent: { status: "idle" | "running" | "pr-closed" | "pr-merged" }; busy: boolean }) => any;
  /** "+" on a project row: start a new agent in that project. */
  onNewAgent: (slug: string) => void;
  /** "+" on the section header: create a project (opens the picker flow). */
  onNewProject: () => void;
}) {
  const [projects, setProjects] = createSignal<ProjectSummary[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [expanded, setExpanded] = createStore<Record<string, boolean>>({});
  /** Per project: whether finished missions are unfolded (default folded). */
  const [showDone, setShowDone] = createStore<Record<string, boolean>>({});
  const LIVE = new Set(["active", "pending", "queued", "awaiting_user", "resuming"]);
  const liveOf = (slug: string) => (missions[slug] ?? []).filter((m) => LIVE.has(m.status));
  const doneOf = (slug: string) => (missions[slug] ?? []).filter((m) => !LIVE.has(m.status));
  // Missions per project slug; file listings per `${slug}:${dirPath}`.
  const [missions, setMissions] = createStore<Record<string, Mission[]>>({});
  const [dirs, setDirs] = createStore<Record<string, ProjectFileEntry[]>>({});
  // The project's controller (Hermes cron), shown as the folder's first row.
  const [controllers, setControllers] = createStore<Record<string, ControllerData>>({});
  const [cronErrors, setCronErrors] = createStore<Record<string, string | null>>({});
  const [cronRetryable, setCronRetryable] = createStore<Record<string, boolean>>({});
  const [cronUnsupported, setCronUnsupported] = createSignal(false);
  const [cronInfo, setCronInfo] = createSignal<string | null>(null);
  const [cronChecking, setCronChecking] = createSignal(false);
  const [cronDefaults, setCronDefaults] = createSignal<import("./api").ProjectCronDefaults | null>(null);
  const [defaultsError, setDefaultsError] = createSignal<string | null>(null);
  const [crons, setCrons] = createStore<Record<string, import("./api").ControllerJob[]>>({});
  const [actionMenu, setActionMenu] = createSignal<{ x: number; y: number; slug: string; path: string } | null>(null);
  const [newFolder, setNewFolder] = createSignal<{ slug: string; path: string } | null>(null);
  const [folderName, setFolderName] = createSignal("");
  const [folderError, setFolderError] = createSignal<string | null>(null);
  const [makingFolder, setMakingFolder] = createSignal(false);
  const [makingCron, setMakingCron] = createSignal(false);
  const [cronWarning, setCronWarning] = createSignal<string | null>(null);
  const [newCron, setNewCron] = createSignal<string | null>(null);
  const [actionFocus, setActionFocus] = createSignal(true);
  const rowTip = useRowTip();
  const currentConnection = (version: number) => isConnected() && connectionVersion() === version;
  const loadController = (slug: string) => {
    if (!isConnected()) return;
    const version = connectionVersion();
    getProjectController(slug, 3)
      .then((view) => { if (currentConnection(version)) setControllers(slug, view); })
      .catch(() => {});
  };
  const missingCronApi = (error: unknown) => error instanceof ApiError && [404, 405].includes(error.status) && !/project not found/i.test(error.detail);
  const cronFailure = (slug: string, error: unknown) => {
    if (missingCronApi(error)) setCronUnsupported(true);
    else { setCronRetryable(slug, !(error instanceof ApiError) || error.status >= 500 || [408, 429].includes(error.status)); setCronErrors(slug, error instanceof Error ? error.message : String(error)); }
  };
  const loadCrons = async (slug: string, force = false) => {
    if (!isConnected() || (cronUnsupported() && !force)) return;
    const version = connectionVersion();
    try {
      const jobs = await listProjectCrons(slug);
      if (!currentConnection(version)) return;
      setCrons(slug, jobs); setCronErrors(slug, null); setCronUnsupported(false);
    } catch (error) { if (currentConnection(version)) cronFailure(slug, error); }
  };
  createEffect(on(connectionVersion, () => {
    setCronUnsupported(false);
    setCronChecking(false);
    setCronInfo(null);
    if (!isConnected()) return;
    for (const slug of Object.keys(expanded)) if (expanded[slug] && !slug.includes(":")) void loadCrons(slug);
  }, { defer: true }));
  const beginCron = async (slug: string) => {
    setActionMenu(null);
    if (!isConnected()) return;
    const version = connectionVersion();
    if (cronUnsupported()) { setCronInfo(slug); return; }
    setCronChecking(true);
    try {
      const defaults = await getProjectCronDefaults(slug);
      if (!currentConnection(version)) return;
      setCronDefaults(defaults); setDefaultsError(null); setNewCron(slug);
    } catch (error) { if (currentConnection(version)) { cronFailure(slug, error); setCronInfo(slug); } }
    finally { if (currentConnection(version)) setCronChecking(false); }
  };

  const refresh = () => {
    if (!isConnected()) return;
    const version = connectionVersion();
    listProjects()
      .then((list) => {
        if (!currentConnection(version)) return;
        setProjects(list);
        setError(null);
      })
      .catch((e) => {
        if (!currentConnection(version)) return;
        const msg = e instanceof Error ? e.message : String(e);
        setError(
          /^(404|405)\b/.test(msg)
            ? "This backend build doesn't expose projects yet — update the core."
            : msg,
        );
      });
  };
  createEffect(on(projectsVersion, () => refresh(), { defer: true }));
  onMount(() => {
    refresh();
    // Mission statuses under expanded projects would otherwise freeze at
    // expand time (the flat "Sandboxed" list polls, this tree didn't).
    const stop = pollWhileVisible(() => {
      if (!isConnected()) return;
      for (const project of projects()) {
        if (!expanded[project.slug]) continue;
        loadMissions(project.slug);
        loadController(project.slug);
        loadCrons(project.slug);
      }
    }, 10000);
    onCleanup(stop);
  });

  const loadMissions = (slug: string) => {
    if (!isConnected()) return;
    const version = connectionVersion();
    listProjectMissions(slug)
      .then((list) => {
        if (!currentConnection(version)) return;
        const merged = mergeById(missions[slug] ?? [], list);
        if (merged !== missions[slug]) setMissions(slug, merged);
        const live = new Set(["active", "pending", "queued", "awaiting_user", "resuming", "running", "starting"]);
        for (const m of merged) if (live.has(m.status)) prefetchTranscript(m.id);
      })
      .catch(() => {
        if (currentConnection(version) && !missions[slug]) setMissions(slug, []);
      });
  };

  const loadDir = (slug: string, path: string, force = false) => {
    if (!isConnected()) return;
    const version = connectionVersion();
    const key = `${slug}:${path}`;
    if (dirs[key] && !force) return;
    listProjectFiles(slug, path)
      .then((entries) => { if (currentConnection(version)) setDirs(key, entries); })
      .catch(() => { if (currentConnection(version)) setDirs(key, []); });
  };

  const beginFolder = (slug: string, path: string) => {
    setActionMenu(null);
    setFolderName("");
    setFolderError(null);
    setNewFolder({ slug, path });
  };
  const createFolder = async () => {
    const target = newFolder();
    const name = folderName().trim();
    if (!target || makingFolder()) return;
    if (!name || name === "." || name === ".." || /[\\/]/.test(name)) {
      setFolderError("Use a folder name without slashes.");
      return;
    }
    setMakingFolder(true);
    setFolderError(null);
    try {
      await mkdirProjectFile(target.slug, target.path ? `${target.path}/${name}` : name);
      loadDir(target.slug, target.path, true);
      setExpanded(target.path ? `${target.slug}:${target.path}` : target.slug, true);
      setNewFolder(null);
    } catch (e) {
      setFolderError(e instanceof Error ? e.message : String(e));
    } finally {
      setMakingFolder(false);
    }
  };
  const menuItems = (slug: string, path: string): MenuEntry[] => [
    { kind: "item", label: "New folder", icon: Ic.FolderIcon, onClick: () => beginFolder(slug, path) },
    { kind: "item", label: "New agent", icon: Ic.NewAgentIcon, onClick: () => p.onNewAgent(slug) },
    { kind: "item", label: cronChecking() ? "Checking crons…" : "New cron", icon: Ic.BellIcon, onClick: () => { if (!cronChecking()) void beginCron(slug); } },
  ];
  const toggleProject = (slug: string) => {
    const next = !expanded[slug];
    setExpanded(slug, next);
    if (next) {
      loadMissions(slug);
      loadDir(slug, "");
      loadController(slug);
      loadCrons(slug);
    }
  };

  const toggleDir = (slug: string, path: string) => {
    const key = `${slug}:${path}`;
    const next = !expanded[key];
    setExpanded(key, next);
    if (next) loadDir(slug, path);
  };

  const DirRows = (dp: { slug: string; path: string; depth: number }) => {
    const entries = () => dirs[`${dp.slug}:${dp.path}`] ?? [];
    return (
      <For each={entries()}>
        {(entry) => {
          const childPath = () => (dp.path ? `${dp.path}/${entry.name}` : entry.name);
          if (entry.kind === "dir") {
            const key = () => `${dp.slug}:${childPath()}`;
            return (
              <>
                <button
                  class="row folder depth"
                  style={{ "--depth": dp.depth + 1 }}
                  {...rowTip.bind(rowDetail(entry.name))}
                  onClick={() => toggleDir(dp.slug, childPath())}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setActionFocus(false);
                    setActionMenu({ x: e.clientX, y: e.clientY, slug: dp.slug, path: childPath() });
                  }}
                >
                  <span class="row-ico"><Show when={expanded[key()]} fallback={<Ic.FolderIcon />}>
                    <Ic.FolderOpenIcon />
                  </Show></span>
                  <span class="row-label">{entry.name}</span>
                </button>
                <Show when={expanded[key()]}>
                  <DirRows slug={dp.slug} path={childPath()} depth={dp.depth + 1} />
                </Show>
              </>
            );
          }
          const id = () => `pf:${dp.slug}:${childPath()}`;
          const tip = rowTip.bind(rowDetail(entry.name));
          return (
            <button
              class={`row file depth ${p.selected() === id() ? "active" : ""}`}
              style={{ "--depth": dp.depth + 1 }}
              {...tip}
              onPointerEnter={(e) => {
                tip.onPointerEnter(e);
                const path = childPath();
                cachePrefetch(`pf:${dp.slug}:${path}`, () => readProjectFile(dp.slug, path).then((text) => cachePut(`pf:${dp.slug}:${path}`, text)));
              }}
              onClick={() => p.open(id())}
            >
              <span class="row-ico"><Ic.FileIcon /></span>
              <span class="row-label">{entry.name}</span>
            </button>
          );
        }}
      </For>
    );
  };

  return (
    <>
      <div class="section section-row">
        <span>Projects</span>
        <button class="section-add" title="New project" onClick={() => p.onNewProject()}>
          <Ic.PlusIcon size={13} />
        </button>
      </div>
      <Show when={error()}>
        <div class="row note">{error()}</div>
      </Show>
      <Show when={cronWarning()}><p class="st-error" role="alert">{cronWarning()}</p></Show>
      <For each={projects()}>
        {(project) => {
          const isOpen = () => !!expanded[project.slug];
          return (
            <div class={`group ${isOpen() ? "has" : ""}`}>
              <div class="row project" onContextMenu={(e) => {
                e.preventDefault();
                setActionFocus(false);
                setActionMenu({ x: e.clientX, y: e.clientY, slug: project.slug, path: "" });
              }}>
                <button class="row-main" aria-expanded={isOpen()} onClick={() => toggleProject(project.slug)}>
                  <span class="row-ico"><Show when={isOpen()} fallback={<Ic.FolderIcon />}>
                    <Ic.FolderOpenIcon />
                  </Show></span>
                  <span class="row-label">{project.title || project.slug}</span>
                </button>
                <button
                  class="row-action"
                  aria-label={`Project actions for ${project.title || project.slug}`}
                  title="Project actions"
                  onPointerDown={(e) => setActionFocus(e.pointerType !== "mouse")}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") setActionFocus(true); }}
                  onClick={(e) => {
                    e.stopPropagation();
                    const box = e.currentTarget.getBoundingClientRect();
                    setActionMenu({ x: Math.max(8, box.right - 176), y: box.bottom + 4, slug: project.slug, path: "" });
                  }}
                >
                  <Ic.PlusIcon size={13} />
                </button>
              </div>
              <Show when={isOpen()}>
                <Show when={controllers[project.slug]?.job}>
                  {(job) => {
                    const ticking = () =>
                      (controllers[project.slug]?.runs ?? []).some((r) => r.status === "running" || r.status === "claimed");
                    return (
                      <button
                        class={`row agent d1 cron ${p.selected() === `c:${project.slug}` ? "active" : ""}`}
                        {...rowTip.bind(rowDetail(job().name, ["Controller"]))}
                        onClick={() => p.open(`c:${project.slug}`)}
                      >
                        <span class="row-ico glyph">
                          <CronGlyph job={job()} running={ticking()} />
                        </span>
                        <span class="row-label">{job().name}</span>
                        <span class="row-machine">
                          <span class="row-machine-name cron-next">
                            {ticking() ? "ticking" : !job().enabled || job().state === "paused" ? "paused" : untilLabel(job().next_run_at, Date.now())}
                          </span>
                        </span>
                      </button>
                    );
                  }}
                </Show>
                <Show when={cronUnsupported() || cronErrors[project.slug]}>
                  <div class="cron-unavailable row d1" role="status" title={cronUnsupported() ? "This backend does not support project crons yet. Update the backend, then check again. Existing project content is unchanged." : `Crons could not refresh. Cached jobs are retained. ${cronErrors[project.slug]}`}>
                    <Ic.BellIcon size={12} />
                    <button class="cron-status-label" onClick={() => setCronInfo(project.slug)}>{cronUnsupported() ? "Crons need backend update" : cronRetryable[project.slug] ? "Crons temporarily unavailable" : "Crons unavailable"}</button>
                    <Show when={!cronUnsupported() && cronRetryable[project.slug]}><button class="cron-retry" aria-label="Retry crons" title="Retry crons" onClick={() => void loadCrons(project.slug, true)}>↻</button></Show>
                  </div>
                </Show>
                <For each={crons[project.slug] ?? []}>
                  {(job) => (
                    <button
                      class={`row agent d1 cron ${p.selected() === `pc:${project.slug}:${job.id}` ? "active" : ""}`}
                      {...rowTip.bind(rowDetail(job.name, ["Cron"]))}
                      onClick={() => p.open(`pc:${project.slug}:${job.id}`)}
                    >
                      <span class="row-ico glyph"><CronGlyph job={job} /></span>
                      <span class="row-label">{job.name}</span>
                      <span class="row-machine"><span class="row-machine-name cron-next">{!job.enabled || job.state === "paused" ? "paused" : untilLabel(job.next_run_at, Date.now())}</span></span>
                    </button>
                  )}
                </For>
                <For each={liveOf(project.slug)}>
                  {(m) => {
                    const tip = rowTip.bind(rowDetail(displayTitle(m.title) || m.id, [missionMachine(m)]));
                    return (
                    <button
                      class={`row agent d1 ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                      {...tip}
                      onPointerEnter={(e) => { tip.onPointerEnter(e); void loadTranscript(m.id); }}
                      onClick={() => p.open(`m:${m.id}`)}
                    >
                      <span class="row-ico glyph">
                        <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                      </span>
                      <span class="row-label">{displayTitle(m.title) || m.id}</span>
                      <MachineBadge name={missionMachine(m)} />
                    </button>
                    );
                  }}
                </For>
                <Show when={doneOf(project.slug).length > 0}>
                  <button
                    class="row done-toggle d1"
                    onClick={() => setShowDone(project.slug, !showDone[project.slug])}
                  >
                    <span class="row-ico"><Ic.ChevronRight size={11} class={`chev ${showDone[project.slug] ? "open" : ""}`} /></span>
                    <span class="row-label">
                      {doneOf(project.slug).length} finished
                    </span>
                  </button>
                  <Show when={showDone[project.slug]}>
                    <For each={doneOf(project.slug)}>
                      {(m) => {
                        const tip = rowTip.bind(rowDetail(displayTitle(m.title) || m.id, [missionMachine(m)]));
                        return (
                        <button
                          class={`row agent done d1 ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                          {...tip}
                          onPointerEnter={(e) => { tip.onPointerEnter(e); void loadTranscript(m.id); }}
                          onClick={() => p.open(`m:${m.id}`)}
                        >
                          <span class="row-ico glyph">
                            <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                          </span>
                          <span class="row-label">{displayTitle(m.title) || m.id}</span>
                          <MachineBadge name={missionMachine(m)} />
                        </button>
                        );
                      }}
                    </For>
                  </Show>
                </Show>
                <DirRows slug={project.slug} path="" depth={0} />
                <Show when={(missions[project.slug]?.length ?? 0) === 0 && (dirs[`${project.slug}:`]?.length ?? 0) === 0}>
                  <div class="row note d1">
                    No missions or files yet.
                  </div>
                </Show>
              </Show>
            </div>
          );
        }}
      </For>
      <Show when={projects().length === 0 && !error()}>
        <div class="row note">No projects on the core backend.</div>
      </Show>
      <Show when={actionMenu()}>
        {(menu) => <PopupMenu {...menu()} focus={actionFocus()} items={menuItems(menu().slug, menu().path)} onClose={() => setActionMenu(null)} />}
      </Show>
      <div ref={rowTip.setCard} id={rowTip.id} class="row-tip" role="tooltip" hidden={!rowTip.tip()} style={rowTip.tip() ? { left: `${rowTip.tip()!.x}px`, top: `${rowTip.tip()!.y}px` } : undefined}>
        <Show when={rowTip.tip()}>{(tip) => (
          <>
            <div class="row-tip-title">{tip().title}</div>
            <For each={tip().meta}>{(line) => <div class="row-tip-meta">{line}</div>}</For>
          </>
        )}</Show>
      </div>
      <Show when={newFolder()}>
        {(target) => (
          <Dialog title="New folder" onClose={() => !makingFolder() && setNewFolder(null)} footer={<><button class="s-btn" disabled={makingFolder()} onClick={() => setNewFolder(null)}>Cancel</button><button class="s-btn primary" disabled={makingFolder()} onClick={createFolder}>{makingFolder() ? "Creating…" : "Create"}</button></>}>
            <Field label={`In ${target().path ? `${target().slug}/${target().path}` : target().slug}`}>
              <input autofocus class="s-input" placeholder="Folder name" value={folderName()} onInput={(e) => setFolderName(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void createFolder(); } }} />
            </Field>
            <Show when={folderError()}><p class="st-error">{folderError()}</p></Show>
          </Dialog>
        )}
      </Show>
      <Show when={cronInfo()}>{(slug) => <Dialog title="Project crons" onClose={() => setCronInfo(null)} footer={<><button class="s-btn sm" onClick={() => setCronInfo(null)}>Close</button><button class="s-btn sm" disabled={cronChecking()} onClick={async () => { if (!isConnected()) return; const version = connectionVersion(); setCronChecking(true); await loadCrons(slug(), true); if (!currentConnection(version)) return; setCronChecking(false); if (!cronUnsupported() && !cronErrors[slug()]) setCronInfo(null); }}>Check again</button></>}>
        <p>{cronUnsupported() ? "This backend does not support project crons yet. Update the connected backend, then choose Check again. Your canonical controller and existing project content remain available." : cronRetryable[slug()] ? "Project crons could not refresh. Previously loaded jobs are retained. Try again when the scheduler is available." : "The backend rejected this cron request. Check backend access and configuration, then check again. Previously loaded jobs are retained."}</p>
      </Dialog>}</Show>
      <Show when={newCron()}>
        {(slug) => <Dialog wide title="New cron" onClose={() => !makingCron() && setNewCron(null)} footer={<span>Unfinished drafts are kept until saved or discarded.</span>}>
          <CronForm creating deliveryRoute={{ ready: cronDefaults()?.route_ready ?? false, loading: !cronDefaults() && !defaultsError(), error: defaultsError() }} onBusyChange={setMakingCron} draftKey={`create:${slug()}`} view={{ slug: slug(), job: { id: "", name: "", schedule: "every 1h", enabled: true, failure_streak: 0 }, runs: [] }}
            save={async (draft) => getProjectCronFromJob(slug(), await createProjectCron(slug(), draft))}
            onClose={() => setNewCron(null)} onSaved={(view, warning) => {
              setCronWarning(warning ? `Cron created. ${warning}` : null);
              setExpanded(slug(), true);
              loadDir(slug(), "", true);
              loadMissions(slug());
              loadController(slug());
              loadCrons(slug());
              p.open(`pc:${slug()}:${view.job!.id}`);
              setNewCron(null);
            }} />
        </Dialog>}

      </Show>
    </>
  );
}

/** Markdown view/editor for a file hosted on the core backend. Autosaves. */
export function ProjectFileView(p: { slug: string; path: string }) {
  const fileKey = () => `pf:${p.slug}:${p.path}`;
  const cached = cachePeek<string>(fileKey());
  const [text, setText] = createSignal<string | null>(cached ?? null);
  const [editing, setEditing] = createSignal(false);
  const [state, setState] = createSignal<"loading" | "saved" | "saving" | "error">(cached != null ? "saved" : "loading");
  const [error, setError] = createSignal<string | null>(null);
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  cacheRemember(fileKey());

  onMount(() => {
    cacheLoad(fileKey(), () => readProjectFile(p.slug, p.path))
      .then((content) => {
        setText(content);
        setState("saved");
      })
      .catch((e) => {
        setError(e instanceof Error ? e.message : String(e));
        setState("error");
      });
  });
  let pending: string | null = null;
  const flush = () => {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = undefined;
    if (pending === null) return;
    const t = pending;
    pending = null;
    writeProjectFile(p.slug, p.path, t)
      .then(() => { cachePut(fileKey(), t); setState("saved"); })
      .catch((e) => {
        setError(e instanceof Error ? e.message : String(e));
        setState("error");
      });
  };
  // Don't lose a debounced edit when the user switches files mid-save.
  onCleanup(flush);

  const onInput = (t: string) => {
    setText(t);
    setState("saving");
    pending = t;
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(flush, 800);
  };

  const name = () => p.path.split("/").pop() ?? p.path;

  return (
    <>
      <div class="pf-bar">
        <span class="pf-path">{p.slug}/{p.path}</span>
        <span class="dlg-spacer" />
        <Show when={state() === "saving"}>
          <span class="pf-state">Saving…</span>
        </Show>
        <Show when={state() === "saved"}>
          <span class="pf-state dim">Saved</span>
        </Show>
        <button class="s-btn" onClick={() => setEditing(!editing())}>
          {editing() ? "Preview" : "Edit"}
        </button>
      </div>
      <Show
        when={editing()}
        fallback={
          <div class="scroll">
            <div class="col">
              <Show when={state() === "error"}>
                <p class="st-error">{error()}</p>
              </Show>
              <Show when={text() !== null} fallback={<FileSkeleton />}>
                <MdView text={text() ?? ""} />
              </Show>
            </div>
          </div>
        }
      >
        <div class="file-view">
          <Show when={text() !== null} fallback={<p class="s-lead shimmer">Loading {name()}…</p>}>
            <MdSource text={text() ?? ""} onInput={onInput} />
          </Show>
        </div>
      </Show>
    </>
  );
}
