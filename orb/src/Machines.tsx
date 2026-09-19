import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { pollWhileVisible } from "./poll";
import { createStore, produce } from "solid-js/store";
import * as Ic from "./icons";
import { readPalomaPub } from "./pubKey";
import { getRemoteNodes, isConnected, type RemoteNodeView } from "./api";

export type Machine = {
  id: string;
  name: string;
  host: string;
  user: string;
  port: number;
  locked?: boolean;
  custom?: boolean;
  note: string;
};

/** Paloma compute fleet that can take missions. From paloma-backends / paloma-ssh-servers. */
export const MACHINES: Machine[] = [
  { id: "local", name: "This Mac", host: "localhost", user: "local", port: 22, locked: true, note: "This computer" },
  { id: "agent-core", name: "agent-core", host: "65.109.98.246", user: "root", port: 22, note: "Control plane · sandboxed.sh" },
  { id: "ashur", name: "ashur", host: "188.40.69.160", user: "root", port: 22, note: "db0.starknet.id · Lean/Docker" },
  { id: "babylon", name: "babylon", host: "54.36.175.109", user: "ubuntu", port: 22, note: "db1.starknet.id · Lean" },
  { id: "nippur", name: "nippur", host: "37.187.92.183", user: "ubuntu", port: 22, note: "db2.starknet.id · Lean/Verity" },
  { id: "old-agent", name: "old-agent", host: "95.216.112.253", user: "root", port: 22, note: "Compute leaf · sandboxed-node" },
  { id: "dgx-spark", name: "dgx-spark", host: "100.77.4.93", user: "th0rgal", port: 22, note: "spark-de79 · Tailscale · GPU" },
];

const CUSTOM_KEY = "orb.customMachines";

function loadCustom(): Machine[] {
  try {
    const raw = localStorage.getItem(CUSTOM_KEY);
    const v: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(v) ? (v as Machine[]).map((m) => ({ ...m, custom: true })) : [];
  } catch {
    return [];
  }
}

function target(m: Machine) {
  if (m.id === "local") return "local";
  return m.port !== 22 ? `${m.user}@${m.host}:${m.port}` : `${m.user}@${m.host}`;
}

function dotClass(status: string) {
  if (status === "online") return "m-dot on";
  if (status === "offline") return "m-dot off";
  return "m-dot warn";
}

function nodeNote(n: RemoteNodeView) {
  const parts = [...n.labels];
  if (n.version) parts.push(n.version);
  if (n.capacity_available != null && n.capacity_total != null) parts.push(`${n.capacity_available}/${n.capacity_total}`);
  return parts.join(" · ");
}

type Draft = { id?: string; name: string; host: string; user: string; port: string; note: string };

const empty = (): Draft => ({ name: "", host: "", user: "ubuntu", port: "22", note: "" });

export function Machines() {
  const [list, setList] = createStore<Machine[]>([...MACHINES.map((m) => ({ ...m })), ...loadCustom()]);
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [pub, setPub] = createSignal("");
  const [copied, setCopied] = createSignal(false);
  const [nodes, setNodes] = createSignal<RemoteNodeView[] | null>(null);

  const persist = () => localStorage.setItem(CUSTOM_KEY, JSON.stringify(list.filter((m) => m.custom)));

  const refresh = async () => {
    try {
      const r = await getRemoteNodes();
      setNodes(r.nodes);
    } catch {
      /* keep last good snapshot */
    }
  };

  onMount(() => {
    void readPalomaPub().then(setPub);
    if (isConnected()) void refresh();
    onCleanup(pollWhileVisible(() => (isConnected() ? refresh() : undefined), 15000));
  });

  const save = () => {
    const d = draft();
    if (!d) return;
    const name = d.name.trim() || d.host.trim();
    const host = d.host.trim();
    if (!name || !host) return;
    const port = Number(d.port) || 22;
    if (d.id) {
      const i = list.findIndex((m) => m.id === d.id);
      if (i >= 0) {
        setList(i, { ...list[i], name, host, user: d.user.trim() || "ubuntu", port, note: d.note.trim() });
      }
    } else {
      setList(list.length, {
        id: "m" + Date.now(),
        name,
        host,
        user: d.user.trim() || "ubuntu",
        port,
        custom: true,
        note: d.note.trim() || "SSH",
      });
    }
    persist();
    setDraft(null);
  };

  const copyKey = async () => {
    const k = pub();
    if (!k) return;
    await navigator.clipboard.writeText(k);
    setCopied(true);
    setTimeout(() => setCopied(false), 1400);
  };

  const editable = () => (isConnected() ? list.filter((m) => m.custom) : list.slice(1));

  const remove = (id: string) => {
    setList(produce((ls) => {
      const i = ls.findIndex((m) => m.id === id);
      if (i >= 0) ls.splice(i, 1);
    }));
    persist();
    setDraft(null);
  };

  /** Inline editor rendered in place of a row (edit) or after the list (add). */
  const Editor = (p: { d: Draft }) => {
    const set = (patch: Partial<Draft>) => setDraft({ ...(draft() ?? p.d), ...patch });
    const keys = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDraft(null);
      else if (e.key === "Enter") save();
    };
    const valid = () => !!(draft()?.host ?? "").trim();
    return (
      <div class="m-edit" onKeyDown={keys}>
        <div class="m-edit-grid">
          <label class="m-field">
            <span>Name</span>
            <input value={p.d.name} placeholder={p.d.host || "agent-core"} autofocus onInput={(e) => set({ name: e.currentTarget.value })} />
          </label>
          <label class="m-field">
            <span>Host</span>
            <input value={p.d.host} placeholder="192.168.1.10" spellcheck={false} onInput={(e) => set({ host: e.currentTarget.value })} />
          </label>
          <label class="m-field">
            <span>User</span>
            <input value={p.d.user} spellcheck={false} onInput={(e) => set({ user: e.currentTarget.value })} />
          </label>
          <label class="m-field narrow">
            <span>Port</span>
            <input value={p.d.port} inputmode="numeric" onInput={(e) => set({ port: e.currentTarget.value })} />
          </label>
          <label class="m-field wide">
            <span>Note</span>
            <input value={p.d.note} placeholder="Optional" onInput={(e) => set({ note: e.currentTarget.value })} />
          </label>
        </div>
        <div class="m-edit-foot">
          <Show when={p.d.id}>
            <button class="s-btn sm quiet danger" onClick={() => remove(p.d.id!)}>Remove</button>
          </Show>
          <span class="dlg-spacer" />
          <button class="s-btn sm quiet" onClick={() => setDraft(null)}>Cancel</button>
          <button class="s-btn sm primary" disabled={!valid()} onClick={save}>{p.d.id ? "Save" : "Add"}</button>
        </div>
      </div>
    );
  };

  return (
    <div class="page">
      <div class="page-head">
        <h2>Machines</h2>
        <button class="s-btn" onClick={() => setDraft(empty())}>
          <Ic.PlusIcon size={14} /> Add
        </button>
      </div>
      <p class="s-lead">
        {isConnected()
          ? "Live sandboxed.sh fleet, refreshed every 15s."
          : "New Agent runs on one of these over Paloma SSH. Connect a backend in Settings to see the live fleet."}
      </p>

      <div class="m-list">
        <div class="m-row">
          <span class="m-dot on" title="This computer" />
          <div class="m-text">
            <div class="m-name">{MACHINES[0].name}</div>
            <div class="m-meta">{target(MACHINES[0])}</div>
            <div class="m-note">{MACHINES[0].note}</div>
          </div>
        </div>

        <Show when={isConnected()}>
          <For each={nodes() ?? []}>
            {(n) => (
              <div class={`m-row ${n.cordoned ? "cordoned" : ""}`}>
                <span class={dotClass(n.status)} title={n.status} />
                <div class="m-text">
                  <div class="m-name">
                    {n.id}
                    <Show when={n.cordoned}>
                      <span class="m-tag">cordoned</span>
                    </Show>
                  </div>
                  <div class="m-meta">{n.base_url}</div>
                  <Show when={nodeNote(n)}>
                    <div class="m-note">{nodeNote(n)}</div>
                  </Show>
                </div>
              </div>
            )}
          </For>
          <Show when={nodes()?.length === 0}>
            <div class="m-note" style={{ padding: "6px 8px" }}>No remote nodes registered.</div>
          </Show>
        </Show>

        <For each={editable()}>
          {(m) => (
            <Show
              when={draft()?.id === m.id && draft()}
              fallback={
                <button
                  class="m-row editable"
                  onClick={() => setDraft({ id: m.id, name: m.name, host: m.host, user: m.user, port: String(m.port), note: m.note })}
                >
                  <span class="m-dot on" title="Paloma SSH" />
                  <div class="m-text">
                    <div class="m-name">{m.name}</div>
                    <div class="m-meta">{target(m)}</div>
                    <Show when={m.note}>
                      <div class="m-note">{m.note}</div>
                    </Show>
                  </div>
                  <span class="m-edit-hint">Edit</span>
                </button>
              }
            >
              {(d) => <Editor d={d()} />}
            </Show>
          )}
        </For>
        <Show when={draft() && !draft()!.id && draft()}>{(d) => <Editor d={d()} />}</Show>
      </div>

      <div class="key-bar">
        <div class="key-bar-text">
          <div class="s-row-title">Paloma public key</div>
          <div class="key-text">{pub() || "Couldn’t read ~/.ssh/paloma.pub"}</div>
        </div>
        <button class="s-btn" disabled={!pub()} onClick={copyKey}>
          {copied() ? "Copied" : "Copy"}
        </button>
      </div>

    </div>
  );
}
