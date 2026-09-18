import { For, Show, createSignal, onMount } from "solid-js";
import { createStore, produce } from "solid-js/store";
import * as Ic from "./icons";
import { Dialog, Field } from "./Dialog";
import { readPalomaPub } from "./pubKey";

export type Machine = {
  id: string;
  name: string;
  host: string;
  user: string;
  port: number;
  locked?: boolean;
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

function target(m: Machine) {
  if (m.id === "local") return "local";
  return m.port !== 22 ? `${m.user}@${m.host}:${m.port}` : `${m.user}@${m.host}`;
}

type Draft = { id?: string; name: string; host: string; user: string; port: string; note: string };

const empty = (): Draft => ({ name: "", host: "", user: "ubuntu", port: "22", note: "" });

export function Machines() {
  const [list, setList] = createStore<Machine[]>(MACHINES.map((m) => ({ ...m })));
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [pub, setPub] = createSignal("");
  const [copied, setCopied] = createSignal(false);

  onMount(() => {
    void readPalomaPub().then(setPub);
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
        note: d.note.trim() || "SSH",
      });
    }
    setDraft(null);
  };

  const copyKey = async () => {
    const k = pub();
    if (!k) return;
    await navigator.clipboard.writeText(k);
    setCopied(true);
    setTimeout(() => setCopied(false), 1400);
  };

  return (
    <div class="page">
      <div class="page-head">
        <h2>Machines</h2>
        <button class="s-btn" onClick={() => setDraft(empty())}>
          <Ic.PlusIcon size={14} /> Add
        </button>
      </div>
      <p class="s-lead">New Agent runs on one of these over Paloma SSH. Nothing is dispatched to sandboxed.sh yet.</p>

      <div class="m-list">
        <For each={list}>
          {(m, i) => (
            <div class="m-row" onClick={() => !m.locked && setDraft({ id: m.id, name: m.name, host: m.host, user: m.user, port: String(m.port), note: m.note })}>
              <span class="m-dot on" title="Paloma SSH" />
              <div class="m-text">
                <div class="m-name">{m.name}</div>
                <div class="m-meta">{target(m)}</div>
                <Show when={m.note}>
                  <div class="m-note">{m.note}</div>
                </Show>
              </div>
              <Show when={!m.locked}>
                <button
                  class="icon-btn m-del"
                  title="Remove"
                  onClick={(e) => {
                    e.stopPropagation();
                    setList(produce((ls) => { ls.splice(i(), 1); }));
                  }}
                >
                  <Ic.CloseIcon size={14} />
                </button>
              </Show>
            </div>
          )}
        </For>
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

      <Show when={draft()}>
        {(d) => (
          <Dialog title={d().id ? "Edit machine" : "Add machine"} onClose={() => setDraft(null)} footer={
            <>
              <Show when={d().id}>
                <button
                  class="s-btn danger"
                  onClick={() => {
                    setList(produce((ls) => {
                      const i = ls.findIndex((m) => m.id === d().id);
                      if (i >= 0) ls.splice(i, 1);
                    }));
                    setDraft(null);
                  }}
                >
                  Remove
                </button>
              </Show>
              <span class="dlg-spacer" />
              <button class="s-btn" onClick={() => setDraft(null)}>Cancel</button>
              <button class="s-btn primary" onClick={save}>{d().id ? "Save" : "Add"}</button>
            </>
          }>
            <Field label="Name">
              <input value={d().name} autofocus onInput={(e) => setDraft({ ...d(), name: e.currentTarget.value })} />
            </Field>
            <Field label="Host">
              <input value={d().host} placeholder="192.168.1.10" onInput={(e) => setDraft({ ...d(), host: e.currentTarget.value })} />
            </Field>
            <div class="field-row">
              <Field label="User">
                <input value={d().user} onInput={(e) => setDraft({ ...d(), user: e.currentTarget.value })} />
              </Field>
              <Field label="Port">
                <input value={d().port} onInput={(e) => setDraft({ ...d(), port: e.currentTarget.value })} />
              </Field>
            </div>
            <Field label="Note">
              <input value={d().note} placeholder="Optional" onInput={(e) => setDraft({ ...d(), note: e.currentTarget.value })} />
            </Field>
          </Dialog>
        )}
      </Show>
    </div>
  );
}
