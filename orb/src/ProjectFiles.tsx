import { For, Show, createSignal, onCleanup, onMount, createEffect, on } from "solid-js";
import { mergeById, pollWhileVisible } from "./poll";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { MdSource, MdView } from "./Markdown";
import {
  isConnected,
  listProjectFiles,
  listProjectMissions,
  listProjects,
  createProjectCron,
  listProjectCrons,
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
import { SchedulePicker } from "./SchedulePicker";

/** Sidebar section listing the core backend's projects with their missions
 * and hosted files. Replaces the demo projects when connected. */
/** Where an agent runs: the workspace/machine name behind a cloud glyph.
 * Per agent, not per project — one project can run on several machines. */
function MachineBadge(p: { name?: string | null }) {
  return (
    <Show when={p.name}>
      <span class="row-machine" title={`Runs on ${p.name}`}>
        <span class="row-machine-name">{p.name}</span>
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
  const [crons, setCrons] = createStore<Record<string, import("./api").ControllerJob[]>>({});
  const [actionMenu, setActionMenu] = createSignal<{ x: number; y: number; slug: string; path: string } | null>(null);
  const [newFolder, setNewFolder] = createSignal<{ slug: string; path: string } | null>(null);
  const [folderName, setFolderName] = createSignal("");
  const [folderError, setFolderError] = createSignal<string | null>(null);
  const [makingFolder, setMakingFolder] = createSignal(false);
  const [newCron, setNewCron] = createSignal<string | null>(null);
  const [cronName, setCronName] = createSignal("");
  const [cronPrompt, setCronPrompt] = createSignal("");
  const [cronSchedule, setCronSchedule] = createSignal("every 1h");
  const [cronDeliver, setCronDeliver] = createSignal("local");
  const [cronModel, setCronModel] = createSignal("");
  const [cronProvider, setCronProvider] = createSignal("");
  const [cronError, setCronError] = createSignal<string | null>(null);
  const [makingCron, setMakingCron] = createSignal(false);
  const loadController = (slug: string) => {
    getProjectController(slug, 3)
      .then((view) => setControllers(slug, view))
      .catch(() => {});
  };
  const loadCrons = (slug: string) => {
    listProjectCrons(slug).then((jobs) => setCrons(slug, jobs)).catch(() => setCrons(slug, []));
  };

  const refresh = () => {
    if (!isConnected()) return;
    listProjects()
      .then((list) => {
        setProjects(list);
        setError(null);
      })
      .catch((e) => {
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
    listProjectMissions(slug)
      .then((list) => {
        const merged = mergeById(missions[slug] ?? [], list);
        if (merged !== missions[slug]) setMissions(slug, merged);
      })
      .catch(() => {
        if (!missions[slug]) setMissions(slug, []);
      });
  };

  const loadDir = (slug: string, path: string, force = false) => {
    const key = `${slug}:${path}`;
    if (dirs[key] && !force) return;
    listProjectFiles(slug, path)
      .then((entries) => setDirs(key, entries))
      .catch(() => setDirs(key, []));
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
      setExpanded(`${target.slug}:${target.path}`, true);
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
    { kind: "item", label: "New cron", icon: Ic.BellIcon, onClick: () => {
      setActionMenu(null);
      setCronName(""); setCronPrompt(""); setCronSchedule("every 1h"); setCronDeliver("local"); setCronModel(""); setCronProvider(""); setCronError(null); setNewCron(slug);
    } },
  ];
  const createCron = async () => {
    const slug = newCron();
    if (!slug || makingCron()) return;
    if (!cronName().trim() || !cronPrompt().trim() || !cronSchedule().trim()) {
      setCronError("Name, instruction, and schedule are required.");
      return;
    }
    setMakingCron(true); setCronError(null);
    try {
      await createProjectCron(slug, { name: cronName().trim(), prompt: cronPrompt().trim(), schedule: cronSchedule().trim(), deliver: cronDeliver().trim() || undefined, model: cronModel().trim() || undefined, provider: cronProvider().trim() || undefined });
      loadController(slug);
      loadCrons(slug);
      setNewCron(null);
    } catch (e) {
      setCronError(e instanceof Error ? e.message : String(e));
    } finally { setMakingCron(false); }
  };

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
      <Show when={newCron()}>
        {(slug) => <Dialog title="New cron" onClose={() => !makingCron() && setNewCron(null)} footer={<><button class="s-btn" disabled={makingCron()} onClick={() => setNewCron(null)}>Cancel</button><button class="s-btn primary" disabled={makingCron()} onClick={createCron}>{makingCron() ? "Creating…" : "Create"}</button></>}>
          <Field label={`Project: ${slug()}`}><input autofocus class="s-input" value={cronName()} placeholder="Cron name" onInput={(e) => setCronName(e.currentTarget.value)} /></Field>
          <Field label="Runs"><SchedulePicker value={cronSchedule()} onChange={setCronSchedule} /></Field>
          <Field label="Instruction"><textarea class="cs-prompt" value={cronPrompt()} placeholder="What should Hermes do on each run?" onInput={(e) => setCronPrompt(e.currentTarget.value)} /></Field>
          <Field label="Delivery"><input class="s-input" value={cronDeliver()} placeholder="local" onInput={(e) => setCronDeliver(e.currentTarget.value)} /></Field>
          <Field label="Model override"><input class="s-input" value={cronModel()} placeholder="Hermes default" onInput={(e) => setCronModel(e.currentTarget.value)} /></Field>
          <Field label="Provider override"><input class="s-input" value={cronProvider()} placeholder="Hermes default" onInput={(e) => setCronProvider(e.currentTarget.value)} /></Field>
          <Show when={cronError()}><p class="st-error">{cronError()}</p></Show>
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
