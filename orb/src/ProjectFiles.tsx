import { For, Show, createSignal, onCleanup, onMount, createEffect, on } from "solid-js";
import { mergeById, pollWhileVisible } from "./poll";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { MdSource, MdView } from "./Markdown";
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

/** Sidebar section listing the core backend's projects with their missions
 * and hosted files. Replaces the demo projects when connected. */
/** Where an agent runs: the workspace/machine name behind a cloud glyph.
 * Per agent, not per project — one project can run on several machines. */
function MachineBadge(p: { name?: string | null }) {
  return (
    <Show when={p.name}>
      <span class="row-machine" title={`Runs on ${p.name}`}>
        <Ic.CloudIcon />
      </span>
    </Show>
  );
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
                  onClick={() => toggleDir(dp.slug, childPath())}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setActionMenu({ x: e.clientX, y: e.clientY, slug: dp.slug, path: childPath() });
                  }}
                >
                  <Show when={expanded[key()]} fallback={<Ic.FolderIcon />}>
                    <Ic.FolderOpenIcon />
                  </Show>
                  <span class="row-label">{entry.name}</span>
                </button>
                <Show when={expanded[key()]}>
                  <DirRows slug={dp.slug} path={childPath()} depth={dp.depth + 1} />
                </Show>
              </>
            );
          }
          const id = () => `pf:${dp.slug}:${childPath()}`;
          return (
            <button
              class={`row file depth ${p.selected() === id() ? "active" : ""}`}
              style={{ "--depth": dp.depth + 1 }}
              onClick={() => p.open(id())}
            >
              <Ic.FileIcon />
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
                setActionMenu({ x: e.clientX, y: e.clientY, slug: project.slug, path: "" });
              }}>
                <button class="row-main" aria-expanded={isOpen()} onClick={() => toggleProject(project.slug)}>
                  <Show when={isOpen()} fallback={<Ic.FolderIcon />}>
                    <Ic.FolderOpenIcon />
                  </Show>
                  <span class="row-label">{project.title || project.slug}</span>
                </button>
                <button
                  class="row-action"
                  aria-label={`Project actions for ${project.title || project.slug}`}
                  title="Project actions"
                  onClick={(e) => {
                    e.stopPropagation();
                    const box = e.currentTarget.getBoundingClientRect();
                    setActionMenu({ x: box.right - 150, y: box.bottom + 4, slug: project.slug, path: "" });
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
                        title="Controller (Hermes cron)"
                        onClick={() => p.open(`c:${project.slug}`)}
                      >
                        <span class="glyph">
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
                      title="Project cron"
                      onClick={() => p.open(`pc:${project.slug}:${job.id}`)}
                    >
                      <span class="glyph"><CronGlyph job={job} /></span>
                      <span class="row-label">{job.name}</span>
                      <span class="row-machine"><span class="row-machine-name cron-next">{!job.enabled || job.state === "paused" ? "paused" : untilLabel(job.next_run_at, Date.now())}</span></span>
                    </button>
                  )}
                </For>
                <For each={liveOf(project.slug)}>
                  {(m) => (
                    <button
                      class={`row agent d1 ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                      onClick={() => p.open(`m:${m.id}`)}
                    >
                      <span class="glyph">
                        <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                      </span>
                      <span class="row-label">{m.title || m.id}</span>
                      <MachineBadge name={m.workspace_name} />
                    </button>
                  )}
                </For>
                <Show when={doneOf(project.slug).length > 0}>
                  <button
                    class="row done-toggle d1"
                    onClick={() => setShowDone(project.slug, !showDone[project.slug])}
                  >
                    <Ic.ChevronRight size={11} class={`chev ${showDone[project.slug] ? "open" : ""}`} />
                    <span class="row-label">
                      {doneOf(project.slug).length} finished
                    </span>
                  </button>
                  <Show when={showDone[project.slug]}>
                    <For each={doneOf(project.slug)}>
                      {(m) => (
                        <button
                          class={`row agent done d2 ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                          onClick={() => p.open(`m:${m.id}`)}
                        >
                          <span class="glyph">
                            <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                          </span>
                          <span class="row-label">{m.title || m.id}</span>
                          <MachineBadge name={m.workspace_name} />
                        </button>
                      )}
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
        {(menu) => <PopupMenu {...menu()} items={menuItems(menu().slug, menu().path)} onClose={() => setActionMenu(null)} />}
      </Show>
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
  const [text, setText] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal(false);
  const [state, setState] = createSignal<"loading" | "saved" | "saving" | "error">("loading");
  const [error, setError] = createSignal<string | null>(null);
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    setText(null);
    setState("loading");
    readProjectFile(p.slug, p.path)
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
      .then(() => setState("saved"))
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
              <Show when={text() !== null} fallback={<p class="s-lead shimmer">Loading {name()}…</p>}>
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
