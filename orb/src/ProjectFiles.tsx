import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { MdSource, MdView } from "./Markdown";
import {
  isConnected,
  listProjectFiles,
  listProjectMissions,
  listProjects,
  readProjectFile,
  writeProjectFile,
  type Mission,
  type ProjectFileEntry,
  type ProjectSummary,
} from "./api";

/** Sidebar section listing the core backend's projects with their missions
 * and hosted files. Replaces the demo projects when connected. */
export function LiveProjectsSection(p: {
  selected: () => string | null;
  open: (id: string) => void;
  missionGlyph: (status: string) => "idle" | "running" | "pr-closed" | "pr-merged";
  StatusGlyph: (props: { agent: { status: "idle" | "running" | "pr-closed" | "pr-merged" }; busy: boolean }) => any;
}) {
  const [projects, setProjects] = createSignal<ProjectSummary[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [expanded, setExpanded] = createStore<Record<string, boolean>>({});
  /** Per project: whether finished missions are unfolded (default folded). */
  const [showDone, setShowDone] = createStore<Record<string, boolean>>({});
  const LIVE = new Set(["active", "pending", "queued", "blocked", "awaiting_user", "resuming"]);
  const liveOf = (slug: string) => (missions[slug] ?? []).filter((m) => LIVE.has(m.status));
  const doneOf = (slug: string) => (missions[slug] ?? []).filter((m) => !LIVE.has(m.status));
  // Missions per project slug; file listings per `${slug}:${dirPath}`.
  const [missions, setMissions] = createStore<Record<string, Mission[]>>({});
  const [dirs, setDirs] = createStore<Record<string, ProjectFileEntry[]>>({});

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
  onMount(() => {
    refresh();
    // Mission statuses under expanded projects would otherwise freeze at
    // expand time (the flat "Sandboxed" list polls, this tree didn't).
    const t = window.setInterval(() => {
      if (!isConnected()) return;
      for (const project of projects()) if (expanded[project.slug]) loadMissions(project.slug);
    }, 10000);
    onCleanup(() => clearInterval(t));
  });

  const loadMissions = (slug: string) => {
    listProjectMissions(slug)
      .then((list) => setMissions(slug, list))
      .catch(() => {
        if (!missions[slug]) setMissions(slug, []);
      });
  };

  const loadDir = (slug: string, path: string) => {
    const key = `${slug}:${path}`;
    if (dirs[key]) return;
    listProjectFiles(slug, path)
      .then((entries) => setDirs(key, entries))
      .catch(() => setDirs(key, []));
  };

  const toggleProject = (slug: string) => {
    const next = !expanded[slug];
    setExpanded(slug, next);
    if (next) {
      loadMissions(slug);
      loadDir(slug, "");
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
                  class="row folder"
                  style={{ "padding-left": `${8 + (dp.depth + 1) * 14}px` }}
                  onClick={() => toggleDir(dp.slug, childPath())}
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
              class={`row file ${p.selected() === id() ? "active" : ""}`}
              style={{ "padding-left": `${8 + (dp.depth + 1) * 14}px` }}
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
      <div class="section">Projects</div>
      <Show when={error()}>
        <div class="row dim" style={{ "font-size": "12px" }}>{error()}</div>
      </Show>
      <For each={projects()}>
        {(project) => {
          const isOpen = () => !!expanded[project.slug];
          return (
            <div class={`group ${isOpen() ? "has" : ""}`}>
              <button class="row project" onClick={() => toggleProject(project.slug)}>
                <Show when={isOpen()} fallback={<Ic.FolderIcon />}>
                  <Ic.FolderOpenIcon />
                </Show>
                <span class="row-label">{project.title || project.slug}</span>
                <Ic.CloudIcon class="dim" />
              </button>
              <Show when={isOpen()}>
                <For each={liveOf(project.slug)}>
                  {(m) => (
                    <button
                      class={`row agent ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                      style={{ "padding-left": "22px" }}
                      onClick={() => p.open(`m:${m.id}`)}
                    >
                      <span class="glyph">
                        <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                      </span>
                      <span class="row-label">{m.title || m.id}</span>
                    </button>
                  )}
                </For>
                <Show when={doneOf(project.slug).length > 0}>
                  <button
                    class="row done-toggle"
                    style={{ "padding-left": "22px" }}
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
                          class={`row agent done ${p.selected() === `m:${m.id}` ? "active" : ""}`}
                          style={{ "padding-left": "36px" }}
                          onClick={() => p.open(`m:${m.id}`)}
                        >
                          <span class="glyph">
                            <p.StatusGlyph agent={{ status: p.missionGlyph(m.status) }} busy={false} />
                          </span>
                          <span class="row-label">{m.title || m.id}</span>
                        </button>
                      )}
                    </For>
                  </Show>
                </Show>
                <DirRows slug={project.slug} path="" depth={0} />
                <Show when={(missions[project.slug]?.length ?? 0) === 0 && (dirs[`${project.slug}:`]?.length ?? 0) === 0}>
                  <div class="row dim" style={{ "padding-left": "22px", "font-size": "12px" }}>
                    No missions or files yet.
                  </div>
                </Show>
              </Show>
            </div>
          );
        }}
      </For>
      <Show when={projects().length === 0 && !error()}>
        <div class="row dim" style={{ "font-size": "12px" }}>No projects on the core backend.</div>
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
