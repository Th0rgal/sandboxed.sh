import { For, Show, Switch, Match, createMemo, createSignal, createEffect, onCleanup, onMount, batch } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { JSX } from "solid-js";
import { projects as seed, LOREM_REPLY, type Agent, type Block, type Turn } from "./data";
import * as Ic from "./icons";
import { Settings, SETTINGS_TABS, type SettingsTab } from "./Settings";
import { MACHINES, Machines } from "./Machines";
import { Providers } from "./Providers";
import { Dialog, Field } from "./Dialog";
import { MenuList, PopupMenu, type MenuEntry } from "./Menu";
import { MdSource, MdView } from "./Markdown";
import { getMissionEvents, storedToStream, streamMission } from "./stream";
import { Transcript, applyStreamEvent, type StreamItem } from "./Transcript";
import { LiveProjectsSection, ProjectFileView } from "./ProjectFiles";
import {
  buildRemoteAgentCommand,
  ensureNodeAgentKey,
  createMission,
  getMission,
  getRemoteNodes,
  isConnected,
  listMissions,
  listProjects,
  cancelMission,
  sendMissionMessage,
  type Mission,
  type ProjectSummary,
  type RemoteNodeView,
} from "./api";

const PAGES = new Set(["settings", "machines", "providers"]);

const MODELS = ["Orb Lorem 4.6 High Fast", "Ipsum 5 Max", "Dolor 4.5 Sonnet", "Auto"];

/** Inline markup: `code`, **bold**, [label](href). Nesting is limited to code inside bold/link. */
function inline(text: string): JSX.Element[] {
  const out: JSX.Element[] = [];
  const re = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)]+)\)/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1] !== undefined) out.push(<strong>{inline(m[1])}</strong>);
    else if (m[2] !== undefined) out.push(<code>{m[2]}</code>);
    else out.push(<a href={m[4]} onClick={(e) => e.preventDefault()}>{inline(m[3])}</a>);
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

function Composer(p: {
  placeholder: string;
  busy: boolean;
  onSend: (t: string) => void;
  onStop: () => void;
  autofocus?: boolean;
  tall?: boolean;
  files?: { id: string; name: string }[];
  attached?: string[];
  onToggleFile?: (id: string) => void;
}) {
  const [text, setText] = createSignal("");
  const [model, setModel] = createSignal(MODELS[0]);
  const [menu, setMenu] = createSignal(false);
  const [ctx, setCtx] = createSignal(false);
  let ta!: HTMLTextAreaElement;
  const resize = () => {
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 220) + "px";
  };
  const send = () => {
    const t = text().trim();
    if (!t || p.busy) return;
    p.onSend(t);
    setText("");
    ta.value = "";
    resize();
  };
  onMount(() => p.autofocus && ta.focus());
  const close = () => {
    setMenu(false);
    setCtx(false);
  };
  onMount(() => window.addEventListener("pointerdown", close));
  onCleanup(() => window.removeEventListener("pointerdown", close));
  const plus = (
    <div class="plus-wrap" onPointerDown={(e) => e.stopPropagation()}>
      <button class="plus" title="Add context" onClick={() => setCtx(!ctx())}>
        <Ic.PlusIcon size={14} />
      </button>
      <Show when={ctx() && (p.files?.length ?? 0) > 0}>
        <div class="menu plus-menu">
          <For each={p.files}>
            {(f) => (
              <button
                class={`menu-item ${p.attached?.includes(f.id) ? "on" : ""}`}
                onClick={() => p.onToggleFile?.(f.id)}
              >
                <span class="menu-ico">
                  <Ic.FileIcon size={14} />
                </span>{" "}
                {f.name}
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
  const modelBtn = (
    <div class="model-wrap" onPointerDown={(e) => e.stopPropagation()}>
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
  );
  const sendBtn = (
    <Show
      when={p.busy}
      fallback={
        <button class="send" onClick={send} title={text().trim() ? "Send" : "Dictate"}>
          <Show when={text().trim()} fallback={<Ic.MicIcon size={15} />}>
            <Ic.ArrowUpIcon size={14} />
          </Show>
        </button>
      }
    >
      <button class="send" onClick={p.onStop} title="Stop">
        <Ic.StopIcon size={14} />
      </button>
    </Show>
  );
  return (
    <div class={`composer ${p.tall ? "tall" : ""}`} onClick={() => ta.focus()}>
      {plus}
      <textarea
        ref={ta}
        rows={1}
        placeholder={p.placeholder}
        onInput={(e) => {
          setText(e.currentTarget.value);
          resize();
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            send();
          }
        }}
      />
      {modelBtn}
      {sendBtn}
    </div>
  );
}

export default function App() {
  const [projects, setProjects] = createStore(structuredClone(seed));
  const [selected, setSelected] = createSignal<string | null>("a1");
  const [collapsed, setCollapsed] = createStore<Record<string, boolean>>({});
  const [sidebar, setSidebar] = createSignal(true);
  const [sbWidth, setSbWidth] = createSignal(220);
  const [streamingId, setStreamingId] = createSignal<string | null>(null);
  const [newProject, setNewProject] = createSignal(seed[0].id);
  const [liveProjects, setLiveProjects] = createSignal<ProjectSummary[]>([]);
  const [newMachine, setNewMachine] = createSignal(MACHINES[0].id);
  const [envOpen, setEnvOpen] = createSignal<"machine" | "project" | null>(null);
  const [history, setHistory] = createSignal<(string | null)[]>(["a1"]);
  const [hIdx, setHIdx] = createSignal(0);
  const [settingsTab, setSettingsTab] = createSignal<SettingsTab>("general");
  const [plusFor, setPlusFor] = createSignal<string | null>(null);
  const [nameDlg, setNameDlg] = createSignal<null | { kind: "folder" | "file" | "rename-project" | "rename-folder" | "rename-file" | "rename-agent"; pid: string; fid?: string; fileId?: string; agentId?: string; value: string }>(null);
  const [attached, setAttached] = createSignal<string[]>([]);
  const [ctx, setCtx] = createSignal<{ x: number; y: number; items: MenuEntry[] } | null>(null);
  const [mdSrc, setMdSrc] = createSignal(false);
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
  const [fleetNodes, setFleetNodes] = createSignal<RemoteNodeView[]>([]);
  const refreshMissions = async () => {
    try {
      setMissions(await listMissions());
    } catch {
      /* keep last good list */
    }
  };
  const refreshFleet = async () => {
    try {
      setFleetNodes((await getRemoteNodes()).nodes);
    } catch {
      /* keep last good list */
    }
  };
  const currentMissionId = createMemo(() => {
    const id = selected();
    return id && id.startsWith("m:") ? id.slice(2) : null;
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
    s === "active" || s === "running" ? "running" : s === "failed" || s === "not_feasible" ? "pr-closed" : "idle";
  const sortedNodes = () => [...fleetNodes()].sort((a, b) => Number(b.status === "online") - Number(a.status === "online"));
  const machineLabel = () => {
    if (isConnected()) {
      if (newMachine() === "core") return "Core (agent-core)";
      return fleetNodes().find((n) => n.id === newMachine())?.id ?? "Core (agent-core)";
    }
    return MACHINES.find((m) => m.id === newMachine())?.name;
  };
  createEffect(() => {
    if (isConnected()) {
      void refreshMissions();
      void refreshFleet();
      listProjects()
        .then(setLiveProjects)
        .catch(() => setLiveProjects([]));
      // Seed agent ids only exist offline — don't land on a demo transcript.
      const sel = selected();
      if (sel && !sel.includes(":") && !["settings", "machines", "providers"].includes(sel)) open(null);
      if (newMachine() !== "core" && !fleetNodes().some((n) => n.id === newMachine())) setNewMachine("core");
    } else if (newMachine() === "core" || fleetNodes().some((n) => n.id === newMachine())) {
      setNewMachine(MACHINES[0].id);
    }
  });
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
  const onSettings = () => selected() === "settings";
  const openSettings = (tab: SettingsTab = "general") => {
    setSettingsTab(tab);
    open("settings");
  };
  const leaveSettings = () => {
    const h = history();
    for (let i = hIdx() - 1; i >= 0; i--) {
      if (h[i] !== "settings") {
        setHIdx(i);
        open(h[i], false);
        return;
      }
    }
    open("a1");
  };

  const toBottom = (smooth = false) =>
    requestAnimationFrame(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: smooth ? "smooth" : "auto" }));

  const open = (id: string | null, push = true) => {
    batch(() => {
      setSelected(id);
      if (push) {
        const h = history().slice(0, hIdx() + 1);
        h.push(id);
        setHistory(h);
        setHIdx(h.length - 1);
      }
    });
    toBottom();
  };
  const nav = (d: number) => {
    const i = hIdx() + d;
    if (i < 0 || i >= history().length) return;
    setHIdx(i);
    open(history()[i], false);
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

  const create = async (text: string) => {
    if (isConnected()) {
      const title = text.length > 42 ? text.slice(0, 42) : text;
      const node = fleetNodes().find((n) => n.id === newMachine());
      const projectSlug = liveProjects().some((p) => p.slug === newProject()) ? newProject() : undefined;
      try {
        const m = await createMission(
          node
            ? { title, prompt: text, remote_node_id: node.id, remote_command: buildRemoteAgentCommand(text, await ensureNodeAgentKey()), project: projectSlug }
            : { title, prompt: text, project: projectSlug },
        );
        await refreshMissions();
        open(`m:${m.id}`);
      } catch {
        /* surfaced by the next missions poll */
      }
      return;
    }
    const id = "n" + Date.now();
    const title = text.length > 42 ? text.slice(0, 42) : text;
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
      if (currentFile()) {
        e.preventDefault();
        setMdSrc(!mdSrc());
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
    const missionsTimer = window.setInterval(() => {
      if (isConnected()) void refreshMissions();
    }, 5000);
    const fleetTimer = window.setInterval(() => {
      if (isConnected()) void refreshFleet();
    }, 15000);
    onCleanup(() => {
      clearInterval(missionsTimer);
      clearInterval(fleetTimer);
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
      <aside class="sidebar">
        <div class="sb-top" data-tauri-drag-region />
        <nav class="sb-scroll" onContextMenu={(e) => e.preventDefault()}>
          <Show
            when={onSettings()}
            fallback={
              <>
                <button class={`row ${selected() === null ? "active" : ""}`} onClick={() => open(null)}>
                  <Ic.NewAgentIcon />
                  <span class="row-label">New Agent</span>
                  <kbd>⌘N</kbd>
                </button>
                <button class={`row ${selected() === "machines" ? "active" : ""}`} onClick={() => open("machines")}>
                  <Ic.MachinesIcon />
                  <span class="row-label">Machines</span>
                </button>
                <button class={`row ${selected() === "providers" ? "active" : ""}`} onClick={() => open("providers")}>
                  <Ic.ProvidersIcon />
                  <span class="row-label">Providers</span>
                </button>

                <Show
                  when={isConnected()}
                  fallback={
                    <>
                      <div class="section">Projects</div>
                      <div class="row dim" style={{ "font-size": "12px" }}>
                        Connect in Settings → Backend to see projects.
                      </div>
                    </>
                  }
                >
                  <LiveProjectsSection selected={selected} open={open} missionGlyph={missionGlyph} StatusGlyph={StatusGlyph} />
                </Show>

                <Show when={isConnected()}>
                  <div class="section">Sandboxed</div>
                  <For each={missions()}>
                    {(m) => (
                      <button
                        class={`row agent ${selected() === `m:${m.id}` ? "active" : ""}`}
                        onClick={() => open(`m:${m.id}`)}
                      >
                        <span class="glyph">
                          <StatusGlyph agent={{ status: missionGlyph(m.status) }} busy={false} />
                        </span>
                        <span class="row-label">{m.title ?? m.id}</span>
                      </button>
                    )}
                  </For>
                  <Show when={missions().length === 0}>
                    <div class="s-lead" style={{ padding: "2px 10px", margin: 0 }}>No missions yet.</div>
                  </Show>
                </Show>
              </>
            }
          >
            <button class="row" onClick={leaveSettings}>
              <Ic.ArrowLeft />
              <span class="row-label">Back</span>
            </button>
            <For each={SETTINGS_TABS}>
              {(t) => (
                <button class={`row ${settingsTab() === t.id ? "active" : ""}`} onClick={() => setSettingsTab(t.id)}>
                  <t.icon />
                  <span class="row-label">{t.label}</span>
                </button>
              )}
            </For>
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
          <button class="icon-btn" title="Toggle sidebar (⌘B)" onClick={() => setSidebar(!sidebar())}>
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
              <span>Settings</span>
            </Match>
            <Match when={selected() === "machines"}>
              <span>Machines</span>
            </Match>
            <Match when={selected() === "providers"}>
              <span>Providers</span>
            </Match>
            <Match when={currentMissionId()}>
              {(id) => (
                <>
                  <span>{missions().find((m) => m.id === id())?.title ?? "Mission"}</span>
                  <Ic.CloudIcon class="dim" />
                </>
              )}
            </Match>
            <Match when={currentProjectFile()}>
              {(pf) => (
                <>
                  <span>{pf().path.split("/").pop()}</span>
                  <Ic.CloudIcon class="dim" />
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
      </header>

      <main class="main">
        <Switch>
          <Match when={selected() === "settings"}>
            <Settings tab={settingsTab()} />
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
                {(mid) => <MissionView id={mid} />}
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
            <div class="new-agent">
              <div class="new-inner">
                <div class="na-meta">
                  <div class="na-drop" onPointerDown={(e) => e.stopPropagation()}>
                    <button class="na-drop-btn" onClick={() => setEnvOpen(envOpen() === "project" ? null : "project")}>
                      {isConnected()
                        ? (liveProjects().find((p) => p.slug === newProject())?.title ?? liveProjects()[0]?.title ?? "No project")
                        : projects.find((p) => p.id === newProject())?.name}
                      <Ic.ChevronDown size={12} />
                    </button>
                    <Show when={envOpen() === "project"}>
                      <div class="menu na-menu">
                        <For each={isConnected() ? liveProjects().map((p) => ({ id: p.slug, name: p.title ?? p.slug })) : projects.map((p) => ({ id: p.id, name: p.name }))}>
                          {(p) => (
                            <button
                              class={`menu-item ${p.id === newProject() ? "on" : ""}`}
                              onClick={() => {
                                setNewProject(p.id);
                                setEnvOpen(null);
                              }}
                            >
                              <span class="menu-ico">
                                <Ic.FolderIcon />
                              </span>
                              {p.name}
                            </button>
                          )}
                        </For>
                      </div>
                    </Show>
                  </div>
                  <div class="na-drop" onPointerDown={(e) => e.stopPropagation()}>
                    <button class="na-drop-btn" onClick={() => setEnvOpen(envOpen() === "machine" ? null : "machine")}>
                      <Show when={newMachine() !== "local"} fallback={<Ic.LaptopIcon size={14} />}>
                        <Ic.MachinesIcon size={14} />
                      </Show>
                      {machineLabel()}
                      <Ic.ChevronDown size={12} />
                    </button>
                    <Show when={envOpen() === "machine"}>
                      <div class="menu na-menu">
                        <Show
                          when={isConnected()}
                          fallback={
                            <>
                              <For each={MACHINES.filter((m) => m.id === "local")}>
                                {(m) => (
                                  <button
                                    class={`menu-item ${m.id === newMachine() ? "on" : ""}`}
                                    onClick={() => {
                                      setNewMachine(m.id);
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
                                      setNewMachine(m.id);
                                      setEnvOpen(null);
                                    }}
                                  >
                                    <span class="menu-ico">
                                      <Ic.MachinesIcon />
                                    </span>
                                    <span class="menu-col">
                                      {m.name}
                                      <span class="menu-sub">{m.user}@{m.host}</span>
                                    </span>
                                  </button>
                                )}
                              </For>
                            </>
                          }
                        >
                          <button
                            class={`menu-item ${newMachine() === "core" ? "on" : ""}`}
                            onClick={() => {
                              setNewMachine("core");
                              setEnvOpen(null);
                            }}
                          >
                            <span class="menu-ico">
                              <Ic.MachinesIcon />
                            </span>
                            <span class="menu-col">
                              Core (agent-core)
                              <span class="menu-sub">Backend host workspace</span>
                            </span>
                          </button>
                          <div class="menu-sep" />
                          <For each={sortedNodes()}>
                            {(n) => (
                              <button
                                class={`menu-item ${n.id === newMachine() ? "on" : ""}`}
                                onClick={() => {
                                  setNewMachine(n.id);
                                  setEnvOpen(null);
                                }}
                              >
                                <span class="menu-ico">
                                  <Ic.MachinesIcon />
                                </span>
                                <span class="menu-col">
                                  {n.id}
                                  <span class="menu-sub">
                                    {n.status}
                                    {n.cordoned ? " · cordoned" : ""}
                                  </span>
                                </span>
                              </button>
                            )}
                          </For>
                        </Show>
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
                    </Show>
                  </div>
                </div>
                <Composer
                  placeholder="Plan, Build, / for commands, @ for context"
                  busy={false}
                  onSend={create}
                  onStop={stop}
                  autofocus
                  tall
                  files={projectFiles()}
                  attached={attached()}
                  onToggleFile={(id) =>
                    setAttached(attached().includes(id) ? attached().filter((x) => x !== id) : [...attached(), id])
                  }
                />
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
                    busy={streamingId() === c.id}
                    onSend={send}
                    onStop={stop}
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
      <Show when={nameDlg()}>
        {(d) => (
          <Dialog
            title={d().kind.startsWith("rename") ? "Rename" : d().kind === "folder" ? "New folder" : "New file"}
            onClose={() => setNameDlg(null)}
            footer={
              <>
                <span class="dlg-spacer" />
                <button class="s-btn" onClick={() => setNameDlg(null)}>Cancel</button>
                <button class="s-btn primary" onClick={confirmName}>{d().kind.startsWith("rename") ? "Save" : "Create"}</button>
              </>
            }
          >
            <Field label="Name">
              <input
                autofocus
                placeholder={d().kind === "folder" ? "notes" : "note.md"}
                value={d().value}
                onInput={(e) => setNameDlg({ ...d(), value: e.currentTarget.value })}
                onKeyDown={(e) => e.key === "Enter" && confirmName()}
              />
            </Field>
          </Dialog>
        )}
      </Show>
    </div>
  );
}

function MissionView(p: { id: string }) {
  const [mission, setMission] = createSignal<Mission | null>(null);
  const [items, setItems] = createSignal<StreamItem[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  let scroller: HTMLDivElement | undefined;
  let nearBottom = true;

  const scrollIfPinned = () => {
    if (nearBottom) scroller?.scrollTo({ top: scroller.scrollHeight });
  };

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
  const resync = async () => {
    try {
      const events = await getMissionEvents(p.id);
      let next: StreamItem[] = [];
      for (const row of events) {
        const ev = storedToStream(row);
        if (ev) next = applyStreamEvent(next, ev);
      }
      setItems(next);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  onMount(() => {
    void Promise.all([refresh(), resync()]).then(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
    const stopStream = streamMission(
      p.id,
      (ev) => {
        if (ev.type === "mission_status_changed" || ev.type === "status") {
          void refresh();
          return;
        }
        setItems((cur) => {
          const next = applyStreamEvent(cur, ev);
          if (next !== cur) queueMicrotask(scrollIfPinned);
          return next;
        });
      },
      () => void resync(),
    );
    // Slow status poll — the stream is authoritative for content, but the
    // composer busy state shouldn't depend on it alone.
    const t = window.setInterval(() => void refresh(), 10000);
    onCleanup(() => {
      stopStream();
      clearInterval(t);
    });
  });

  const busy = () => {
    const s = mission()?.status;
    return s === "active" || s === "running";
  };

  const viewItems = () => {
    const list = items();
    if (busy()) return list;
    // Terminal mission: force-close any bubble left open by a dropped
    // assistant_message finalizer.
    return list.map((i) => (i.kind === "text" && i.live ? { ...i, live: false } : i));
  };

  const sendMsg = (text: string) => {
    void sendMissionMessage(p.id, text)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .then(() => refresh());
  };

  const stopM = () => {
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
        <div class="col">
          <Transcript items={viewItems()} />
          <Show when={busy() && !items().some((i) => (i.kind === "text" && i.live) || (i.kind === "think" && !i.done))}>
            <p class="s-lead shimmer">Working…</p>
          </Show>
          <Show when={error()}>
            <p class="s-lead">{error()}</p>
          </Show>
        </div>
      </div>
      <div class="dock">
        <div class="col">
          <Composer placeholder="Send follow-up" busy={busy()} onSend={sendMsg} onStop={stopM} />
        </div>
      </div>
    </>
  );
}
