import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createSignal, onCleanup, onMount, createEffect, on } from "solid-js";
import { mergeById, pollWhileVisible } from "./poll";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { MdSource, MdView, mdSource, setMdSource } from "./Markdown";
import { displayTitle } from "./goal";
import { nodeLabel } from "./missionLaunch";
import {
  isConnected,
  ApiError,
  connectionVersion,
  listProjectFiles,
  listProjectMissions,
  listProjects,
  updateProject,
  archiveProject,
  bumpProjects,
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
import { Dialog, PromptSheet } from "./Dialog";
import { PopupMenu, type MenuEntry } from "./Menu";
import { copyText } from "./clipboard";
import { CronForm } from "./ControllerSettings";
import { getProjectCronFromJob } from "./cronSchema";
import { loadTranscript, prefetchTranscript } from "./missionCache";
import { cacheCanPrefetch, cacheLoad, cachePeek, cachePrefetch, cachePut, cacheRemember, prefetchProjectLimit } from "./pageCache";
import { SidebarTree } from "./Tree";
import type { TreeNode, TreeRow } from "./treeModel";
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

/** Prefer overlapping the row's trailing edge (Cursor); otherwise below. Clamp to the viewport. */
export function placeRowTip(
  row: { top: number; left: number; right: number; bottom: number },
  size: { width: number; height: number },
  view: { width: number; height: number },
  gap = 8,
) {
  const pad = 8;
  const overlap = Math.min(32, Math.max(12, row.right - row.left - 40));
  const start = row.right - overlap;
  const beside = view.width - start - pad >= Math.min(size.width, 120);
  const x = beside ? start : row.left;
  const y = beside ? row.top : row.bottom + gap;
  return {
    x: Math.max(pad, Math.min(x, view.width - size.width - pad)),
    y: Math.max(pad, Math.min(y, view.height - size.height - pad)),
  };
}

/**
 * The id to put on the clipboard for a sidebar agent row: the sandboxed mission
 * UUID exactly as the core stores it. Never the `m:` sidebar routing prefix,
 * never the durable remote job id (`remote_job.job_id`), and never a harness
 * session id — those identify an execution attempt, not the mission the
 * `/api/control/missions/:id` endpoints take.
 */
export function missionCopyId(mission: { id: string }): string {
  return mission.id;
}

/** Default extension for a new reference file: these are Markdown notes. */
export const REFERENCE_FILE_EXT = ".md";

/**
 * Validate a new file name typed into the sidebar and return the path relative
 * to `dir`. Traversal, absolute paths and Windows separators are refused here
 * as well as by the core's `safe_join`, so the user sees why instead of a 400.
 * A name with no extension gets `.md`, matching the Markdown view/editor these
 * reference files are read in.
 */
export function newFilePath(dir: string, raw: string): { path: string; name: string } | { error: string } {
  const name = raw.trim();
  if (!name) return { error: "Enter a file name." };
  if (name.includes("\\")) return { error: "Use forward slashes, not backslashes." };
  if (name.startsWith("/")) return { error: "Use a path relative to this folder." };
  const parts = name.split("/");
  if (parts.some((part) => part.trim() === "")) return { error: "Remove the empty path segment." };
  if (parts.some((part) => part.trim() === "." || part.trim() === "..")) {
    return { error: "Paths cannot contain '.' or '..' segments." };
  }
  const cleaned = parts.map((part) => part.trim());
  const last = cleaned[cleaned.length - 1];
  // A dotfile ("`.gitignore`") is already named; only an extensionless name
  // gets the Markdown default.
  cleaned[cleaned.length - 1] = last.includes(".") ? last : `${last}${REFERENCE_FILE_EXT}`;
  return { path: dir ? `${dir}/${cleaned.join("/")}` : cleaned.join("/"), name: cleaned[cleaned.length - 1] };
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
  open: (id: string | null) => void;
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
  const LIVE = new Set(["active", "pending", "queued", "awaiting_user", "resuming", "running", "starting"]);
  const liveOf = (slug: string) => (missions[slug] ?? []).filter((m) => LIVE.has(m.status));
  const doneOf = (slug: string) => (missions[slug] ?? []).filter((m) => !LIVE.has(m.status));
  // Missions per project slug; file listings per `${slug}:${dirPath}`.
  const [missions, setMissions] = createStore<Record<string, Mission[]>>({});
  const [dirErrors, setDirErrors] = createStore<Record<string, string | null>>({});
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
  const [newFile, setNewFile] = createSignal<{ slug: string; path: string } | null>(null);
  const [fileName, setFileName] = createSignal("");
  const [fileError, setFileError] = createSignal<string | null>(null);
  const [makingFile, setMakingFile] = createSignal(false);
  /** Right-click menu on an agent row. Opening it never changes the selection. */
  const [missionMenu, setMissionMenu] = createSignal<{ x: number; y: number; mission: Mission } | null>(null);
  const [copied, setCopied] = createSignal<string | null>(null);
  const [makingCron, setMakingCron] = createSignal(false);
  const [cronWarning, setCronWarning] = createSignal<string | null>(null);
  const [newCron, setNewCron] = createSignal<string | null>(null);
  const [actionFocus, setActionFocus] = createSignal(true);
  const [rename, setRename] = createSignal<{ slug: string; title: string } | null>(null);
  const [renameValue, setRenameValue] = createSignal("");
  const [renameError, setRenameError] = createSignal<string | null>(null);
  const [renaming, setRenaming] = createSignal(false);
  const [actionError, setActionError] = createSignal<string | null>(null);
  const rowTip = useRowTip();
  const currentConnection = (version: number) => isConnected() && connectionVersion() === version;
  const warmed = new Set<string>();
  const warmupQueue: string[] = [];
  let warmupActive = 0;
  const loadController = (slug: string) => {
    if (!isConnected()) return Promise.resolve();
    const version = connectionVersion();
    return getProjectController(slug, 3)
      .then((view) => { if (currentConnection(version)) setControllers(slug, view); })
      .catch(() => {});
  };
  const missingCronApi = (error: unknown) => error instanceof ApiError && [404, 405].includes(error.status) && !/project not found/i.test(error.detail);
  const cronFailure = (slug: string, error: unknown) => {
    if (missingCronApi(error)) setCronUnsupported(true);
    else { setCronRetryable(slug, !(error instanceof ApiError) || error.status >= 500 || [408, 429].includes(error.status)); setCronErrors(slug, error instanceof Error ? error.message : String(error)); }
  };
  const cronLoads = new Map<string, Promise<void>>();
  const loadCrons = (slug: string, force = false): Promise<void> => {
    if (!isConnected() || (cronUnsupported() && !force)) return Promise.resolve();
    const version = connectionVersion();
    const key = `${version}:${slug}`;
    const pending = cronLoads.get(key);
    if (pending) return pending;
    const request = (async () => {
      try {
        const jobs = await listProjectCrons(slug);
        if (!currentConnection(version)) return;
        setCrons(slug, jobs); setCronErrors(slug, null); setCronUnsupported(false);
      } catch (error) { if (currentConnection(version)) cronFailure(slug, error); }
      finally { cronLoads.delete(key); }
    })();
    cronLoads.set(key, request);
    return request;
  };
  createEffect(on(connectionVersion, () => {
    setCronUnsupported(false);
    setCronChecking(false);
    setCronInfo(null);
    warmed.clear();
    warmupQueue.length = 0;
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
        warmupProjects(list);
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

  const loadMissions = (slug: string, opts?: { transcripts?: boolean }) => {
    if (!isConnected()) return Promise.resolve();
    const version = connectionVersion();
    return listProjectMissions(slug)
      .then((list) => {
        if (!currentConnection(version)) return;
        const merged = mergeById(missions[slug] ?? [], list);
        if (merged !== missions[slug]) setMissions(slug, merged);
        if (opts?.transcripts === false) return;
        const live = new Set(["active", "pending", "queued", "awaiting_user", "resuming", "running", "starting"]);
        for (const m of merged) if (live.has(m.status)) prefetchTranscript(m.id);
      })
      .catch(() => {
        if (currentConnection(version) && !missions[slug]) setMissions(slug, []);
      });
  };

  const loadDir = (slug: string, path: string, force = false) => {
    if (!isConnected()) return Promise.resolve();
    const version = connectionVersion();
    const key = `${slug}:${path}`;
    if (dirs[key] && !force) return Promise.resolve();
    setDirErrors(key, null);
    return listProjectFiles(slug, path)
      .then((entries) => { if (currentConnection(version)) setDirs(key, entries); })
      .catch((e) => { if (currentConnection(version)) setDirErrors(key, e instanceof Error ? e.message : String(e)); });
  };

  const warmupOne = async (slug: string) => {
    await Promise.all([
      loadMissions(slug, { transcripts: false }),
      loadDir(slug, ""),
      loadController(slug),
      loadCrons(slug),
    ]);
  };
  const pumpWarmup = () => {
    if (!cacheCanPrefetch()) return;
    while (warmupActive < 2 && warmupQueue.length) {
      const slug = warmupQueue.shift()!;
      warmupActive++;
      void warmupOne(slug).finally(() => {
        warmupActive--;
        pumpWarmup();
      });
    }
  };
  const warmupProjects = (list: ProjectSummary[]) => {
    const limit = prefetchProjectLimit();
    if (!limit) return;
    for (const p of list.slice(0, limit)) {
      if (warmed.has(p.slug)) continue;
      warmed.add(p.slug);
      warmupQueue.push(p.slug);
    }
    pumpWarmup();
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
  const beginFile = (slug: string, path: string) => {
    setActionMenu(null);
    setFileName("");
    setFileError(null);
    setNewFile({ slug, path });
  };
  /**
   * Create an empty reference file through the core's project-file API
   * (`PUT /api/projects/:slug/file`), which stores it under the backend's own
   * `.sandboxed-sh/project-files/<slug>` tree. No mission, workspace or
   * execution machine is involved, and no cron API is touched.
   */
  const createFile = async () => {
    const target = newFile();
    if (!target || makingFile()) return;
    const resolved = newFilePath(target.path, fileName());
    if ("error" in resolved) { setFileError(resolved.error); return; }
    setMakingFile(true);
    setFileError(null);
    try {
      // Re-list the parent rather than trusting the cached rows: another client
      // (or a mission) may have added the file since this listing was loaded,
      // and `writeProjectFile` would overwrite it without asking.
      const parent = resolved.path.slice(0, Math.max(0, resolved.path.lastIndexOf("/")));
      const siblings = await listProjectFiles(target.slug, parent);
      if (siblings.some((entry) => entry.name === resolved.name)) {
        setFileError(`"${resolved.name}" already exists here. Choose another name.`);
        return;
      }
      await writeProjectFile(target.slug, resolved.path, "");
      // Reveal it: refresh the parent listing, unfold every folder on the way
      // down, then open the file in the Markdown view.
      await loadDir(target.slug, parent, true);
      setExpanded(target.slug, true);
      const segments = parent ? parent.split("/") : [];
      for (let i = 1; i <= segments.length; i++) setExpanded(`${target.slug}:${segments.slice(0, i).join("/")}`, true);
      setNewFile(null);
      p.open(`pf:${target.slug}:${resolved.path}`);
    } catch (e) {
      setFileError(e instanceof Error ? e.message : String(e));
    } finally {
      setMakingFile(false);
    }
  };
  const copyMissionId = async (mission: Mission) => {
    setMissionMenu(null);
    setActionError(null);
    const id = missionCopyId(mission);
    try {
      await copyText(id);
      setCopied(id);
      window.setTimeout(() => setCopied((cur) => (cur === id ? null : cur)), 1600);
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  };
  const beginRename = (slug: string) => {
    const project = projects().find((x) => x.slug === slug);
    const title = (project?.title || slug).trim();
    setRename({ slug, title });
    setRenameValue(title);
    setRenameError(null);
  };
  const saveRename = async () => {
    const target = rename();
    const title = renameValue().trim();
    if (!target || renaming()) return;
    if (!title) { setRenameError("Enter a name."); return; }
    setRenaming(true);
    setRenameError(null);
    try {
      await updateProject({ slug: target.slug, title });
      bumpProjects();
      setRename(null);
    } catch (e) {
      setRenameError(e instanceof Error ? e.message : String(e));
    } finally {
      setRenaming(false);
    }
  };
  const archive = async (slug: string) => {
    setActionError(null);
    try {
      await archiveProject(slug);
      setProjects((list) => list.filter((x) => x.slug !== slug));
      bumpProjects();
      const sel = p.selected();
      if (sel === `c:${slug}` || sel?.startsWith(`pc:${slug}:`) || sel?.startsWith(`pf:${slug}:`)) p.open(null);
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  };
  /**
   * Actions for a project row (`path` empty) or one of its folders. A folder
   * only holds files, so it offers file/folder creation; starting an agent or a
   * cron is a project-level act and stays on the project row.
   */
  const menuItems = (slug: string, path: string): MenuEntry[] => {
    const items: MenuEntry[] = [
      { kind: "item", label: "New file", icon: Ic.FileIcon, onClick: () => beginFile(slug, path) },
      { kind: "item", label: "New folder", icon: Ic.FolderIcon, onClick: () => beginFolder(slug, path) },
    ];
    if (!path) {
      items.push(
        { kind: "sep" },
        { kind: "item", label: "New agent", icon: Ic.NewAgentIcon, onClick: () => p.onNewAgent(slug) },
        { kind: "item", label: cronChecking() ? "Checking crons…" : "New cron", icon: Ic.BellIcon, onClick: () => { if (!cronChecking()) void beginCron(slug); } },
        { kind: "sep" },
        { kind: "item", label: "Project settings", icon: Ic.SlidersIcon, onClick: () => { setActionMenu(null); p.open(`ps:${slug}`); } },
        { kind: "item", label: "Rename", icon: Ic.PencilIcon, onClick: () => beginRename(slug) },
        { kind: "item", label: "Archive", icon: Ic.ArchiveIcon, onClick: () => void archive(slug) },
      );
    }
    return items;
  };
  /** Right-click on an agent row: identity actions only, no navigation. */
  const missionMenuItems = (mission: Mission): MenuEntry[] => [
    { kind: "item", label: "Copy mission ID", icon: Ic.CopyIcon, onClick: () => void copyMissionId(mission) },
  ];
  /** Right-click handler shared by every agent row. Suppresses the native menu
   * and the sidebar-wide one without activating the row, so the open agent and
   * the current selection are untouched. */
  const onMissionContext = (e: MouseEvent, mission: Mission) => {
    e.preventDefault();
    e.stopPropagation();
    setActionMenu(null);
    setMissionMenu({ x: e.clientX, y: e.clientY, mission });
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

  type RowData = {
    kind: "project" | "folder" | "file" | "mission" | "finished" | "cron" | "cron-error" | "note";
    slug: string; label: string; path?: string; mission?: Mission;
    job?: import("./api").ControllerJob; controller?: boolean;
  };
  type Node = TreeNode<RowData>;
  const fileNodes = (slug: string, path: string): Node[] => {
    const key = `${slug}:${path}`;
    if (dirErrors[key]) return [{ id: `error:${key}`, data: { kind: "note", slug, path, label: `Files unavailable: ${dirErrors[key]}` } }];
    if (!dirs[key]) return [{ id: `loading:${key}`, data: { kind: "note", slug, label: "Loading files…" } }];
    if (!dirs[key].length) return path ? [{ id: `empty:${key}`, data: { kind: "note", slug, label: "Empty folder" } }] : [];
    return dirs[key].map(entry => {
      const childPath = path ? `${path}/${entry.name}` : entry.name;
      const open = !!expanded[`${slug}:${childPath}`];
      return { id: `pf:${slug}:${childPath}`, data: { kind: entry.kind === "dir" ? "folder" : "file", slug, path: childPath, label: entry.name },
        ...(entry.kind === "dir" ? { expanded: open, children: open ? fileNodes(slug, childPath) : [] } : {}) };
    });
  };
  const missionNode = (slug: string, mission: Mission): Node => ({ id: `m:${mission.id}`, data: { kind: "mission", slug, mission, label: displayTitle(mission.title) || mission.id } });
  const tree = (): Node[] => projects().map(project => {
    const slug = project.slug, open = !!expanded[slug];
    const children: Node[] = [];
    if (open) {
      const job = controllers[slug]?.job;
      if (job) children.push({ id: `c:${slug}`, data: { kind: "cron", slug, label: job.name, job, controller: true } });
      if (cronUnsupported() || cronErrors[slug]) children.push({ id: `crons-error:${slug}`, data: { kind: "cron-error", slug, label: "Crons unavailable" } });
      children.push(...(crons[slug] ?? []).map(job => ({ id: `pc:${slug}:${job.id}`, data: { kind: "cron" as const, slug, label: job.name, job } })));
      if (missions[slug] === undefined) children.push({ id: `loading-missions:${slug}`, data: { kind: "note", slug, label: "Loading missions…" } });
      children.push(...liveOf(slug).map(m => missionNode(slug, m)));
      const done = doneOf(slug);
      if (done.length) children.push({ id: `finished:${slug}`, data: { kind: "finished", slug, label: `${done.length} finished` }, expanded: !!showDone[slug], children: done.map(m => missionNode(slug, m)) });
      children.push(...fileNodes(slug, ""));
      if (!children.length) children.push({ id: `empty:${slug}`, data: { kind: "note", slug, label: "No missions or files yet." } });
    }
    return { id: `project:${slug}`, data: { kind: "project", slug, label: project.title || slug }, expanded: open, children };
  });
  const renderRow = (row: TreeRow<RowData>) => {
    const d = row.data;
    const contextMenu = (e: MouseEvent) => {
      e.preventDefault(); e.stopPropagation(); setMissionMenu(null); setActionFocus(false);
      setActionMenu({ x: e.clientX, y: e.clientY, slug: d.slug, path: d.path ?? "" });
    };
    if (d.kind === "project") return <div class="row project" onContextMenu={contextMenu}>
      <button class="row-main" aria-expanded={row.expanded} onClick={() => toggleProject(d.slug)}>
        <span class="row-ico"><Show when={row.expanded} fallback={<Ic.FolderIcon />}><Ic.FolderOpenIcon /></Show></span>
        <span class="row-label">{d.label}</span>
      </button>
      <button class="row-action" aria-label={`Project actions for ${d.label}`} title="Project actions"
        onPointerDown={e => setActionFocus(e.pointerType !== "mouse")}
        onKeyDown={e => { if (e.key === "Enter" || e.key === " ") setActionFocus(true); }}
        onClick={e => { e.stopPropagation(); const box = e.currentTarget.getBoundingClientRect(); setActionMenu({ x: Math.max(8, box.right - 176), y: box.bottom + 4, slug: d.slug, path: "" }); }}><Ic.PlusIcon size={13} /></button>
    </div>;
    if (d.kind === "note") return <div class="row note" role="status">{d.label}<Show when={d.path !== undefined}><button onClick={() => void loadDir(d.slug, d.path!, true)}>Retry</button></Show></div>;
    if (d.kind === "cron-error") return <div class="cron-unavailable row" role="status" title={cronUnsupported() ? "This backend does not support project crons yet. Update the backend, then check again. Existing project content is unchanged." : `Crons could not refresh. Cached jobs are retained. ${cronErrors[d.slug]}`}>
      <Ic.BellIcon size={12} /><button class="cron-status-label" onClick={() => setCronInfo(d.slug)}>{cronUnsupported() ? "Crons need backend update" : cronRetryable[d.slug] ? "Crons temporarily unavailable" : "Crons unavailable"}</button>
      <Show when={!cronUnsupported() && cronRetryable[d.slug]}><button class="cron-retry" aria-label="Retry crons" title="Retry crons" onClick={() => void loadCrons(d.slug, true)}>↻</button></Show>
    </div>;
    if (d.kind === "finished") return <button class="row done-toggle" aria-expanded={row.expanded} onClick={() => setShowDone(d.slug, !showDone[d.slug])}>
      <span class="row-ico"><Show when={row.expanded} fallback={<Ic.FinishedIcon />}><Ic.FinishedOpenIcon /></Show></span><span class="row-label">{d.label}</span>
    </button>;
    if (d.kind === "folder") return <div class="row folder" onContextMenu={contextMenu}>
      <button class="row-main" aria-expanded={row.expanded} {...rowTip.bind(rowDetail(d.label))} onClick={() => toggleDir(d.slug, d.path!)}>
        <span class="row-ico"><Show when={row.expanded} fallback={<Ic.FolderIcon />}><Ic.FolderOpenIcon /></Show></span><span class="row-label">{d.label}</span>
      </button>
      <button class="row-action" aria-label={`New file in ${d.label}`} title="New file"
        onPointerDown={e => setActionFocus(e.pointerType !== "mouse")}
        onKeyDown={e => { if (e.key === "Enter" || e.key === " ") setActionFocus(true); }}
        onClick={e => { e.stopPropagation(); const box = e.currentTarget.getBoundingClientRect(); setActionMenu({ x: Math.max(8, box.right - 176), y: box.bottom + 4, slug: d.slug, path: d.path! }); }}><Ic.PlusIcon size={13} /></button>
    </div>;
    if (d.kind === "cron") {
      const ticking = () => d.controller && (controllers[d.slug]?.runs ?? []).some(r => r.status === "running" || r.status === "claimed");
      return <button class={`row agent cron ${p.selected() === row.id ? "active" : ""}`} {...rowTip.bind(rowDetail(d.label, [d.controller ? "Controller" : "Cron"]))} onClick={() => p.open(row.id)}>
        <span class="row-ico glyph"><CronGlyph job={d.job!} running={!!ticking()} /></span><span class="row-label">{d.label}</span>
        <span class="row-machine"><Show when={!d.job!.enabled || d.job!.state === "paused"} fallback={<span class="row-machine-name cron-next">{ticking() ? "ticking" : untilLabel(d.job!.next_run_at, Date.now())}</span>}>
          <span class="cron-paused-indicator" role="img" aria-label="Paused" title={ticking() ? "Paused · current run finishing" : "Paused"}><Ic.PauseIcon size={14} /></span>
        </Show></span>
      </button>;
    }
    const tip = rowTip.bind(rowDetail(d.label, [d.mission ? missionMachine(d.mission) : undefined]));
    return <button class={`row ${d.kind === "mission" ? "agent" : "file"} ${d.mission && !LIVE.has(d.mission.status) ? "done" : ""} ${p.selected() === row.id ? "active" : ""}`} {...tip}
      onPointerEnter={e => { tip.onPointerEnter(e); if (d.mission) void loadTranscript(d.mission.id).catch(() => {}); else cachePrefetch(row.id, () => readProjectFile(d.slug, d.path!).then(text => cachePut(row.id, text))); }} onContextMenu={e => { if (d.mission) onMissionContext(e, d.mission); }} onClick={() => p.open(row.id)}>
      <span class="row-ico glyph"><Show when={d.mission} fallback={<Ic.FileIcon />}>{m => <p.StatusGlyph agent={{ status: p.missionGlyph(m().status) }} busy={false} />}</Show></span>
      <span class="row-label">{d.label}</span><MachineBadge name={d.mission ? missionMachine(d.mission) : undefined} />
    </button>;
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
      <Show when={cronWarning()}><ErrorNotice error={cronWarning()!} /></Show>
      <Show when={actionError()}><ErrorNotice error={actionError()!} /></Show>
      <SidebarTree nodes={tree()} label="Projects" selected={p.selected()} render={renderRow} />
      <Show when={projects().length === 0 && !error()}>
        <div class="row note">No projects on the core backend.</div>
      </Show>
      <Show when={actionMenu()}>
        {(menu) => <PopupMenu {...menu()} focus={actionFocus()} items={menuItems(menu().slug, menu().path)} onClose={() => setActionMenu(null)} />}
      </Show>
      <Show when={missionMenu()}>
        {(menu) => <PopupMenu x={menu().x} y={menu().y} focus={false} items={missionMenuItems(menu().mission)} onClose={() => setMissionMenu(null)} />}
      </Show>
      <Show when={copied()}>
        {(id) => <div class="row note copied-note" role="status">Copied mission ID {id()}</div>}
      </Show>
      <div ref={rowTip.setCard} id={rowTip.id} class="row-tip" role="tooltip" hidden={!rowTip.tip()} style={rowTip.tip() ? { left: `${rowTip.tip()!.x}px`, top: `${rowTip.tip()!.y}px` } : undefined}>
        <Show when={rowTip.tip()}>{(tip) => (
          <>
            <div class="row-tip-title">{tip().title}</div>
            <For each={tip().meta}>{(line) => <div class="row-tip-meta">{line}</div>}</For>
          </>
        )}</Show>
      </div>
      <Show when={rename()}>
        {(target) => (
          <PromptSheet
            title="Rename"
            hint={target().slug}
            label="Project name"
            placeholder="Project name"
            value={renameValue()}
            onInput={setRenameValue}
            action="Save"
            busy={renaming()}
            disabled={!renameValue().trim()}
            error={renameError()}
            onAction={() => void saveRename()}
            onClose={() => !renaming() && setRename(null)}
          />
        )}
      </Show>
      <Show when={newFolder()}>
        {(target) => (
          <PromptSheet
            title="New folder"
            hint={`in ${target().path ? `${target().slug}/${target().path}` : target().slug}`}
            label="Folder name"
            placeholder="Folder name"
            value={folderName()}
            onInput={setFolderName}
            action="Create"
            busy={makingFolder()}
            disabled={!folderName().trim()}
            error={folderError()}
            onAction={() => void createFolder()}
            onClose={() => !makingFolder() && setNewFolder(null)}
          />
        )}
      </Show>
      <Show when={newFile()}>
        {(target) => (
          <PromptSheet
            title="New file"
            hint={`in ${target().path ? `${target().slug}/${target().path}` : target().slug}`}
            label="File name"
            placeholder={`notes${REFERENCE_FILE_EXT}`}
            value={fileName()}
            onInput={setFileName}
            action="Create"
            busy={makingFile()}
            disabled={!fileName().trim()}
            error={fileError()}
            onAction={() => void createFile()}
            onClose={() => !makingFile() && setNewFile(null)}
            footer={<span>Markdown by default — a name with no extension gets {REFERENCE_FILE_EXT}.</span>}
          />
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
  // Shared with the ⌘/ handler in App.tsx; the button and the shortcut drive
  // the same state, so they can never disagree.
  const editing = mdSource;
  const setEditing = setMdSource;
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
        <button class="s-btn" title="Toggle source and preview (⌘/)" onClick={() => setEditing(!editing())}>
          {editing() ? "Preview" : "Edit"}
        </button>
      </div>
      <Show
        when={editing()}
        fallback={
          <div class="scroll">
            <div class="col">
              <Show when={state() === "error"}>
                <ErrorNotice error={error()!} />
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
