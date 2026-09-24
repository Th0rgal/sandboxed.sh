import { ExecutionSettings } from "./ExecutionSettings";
import { Select } from "./Select";
import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createSignal, type JSX } from "solid-js";
import * as Ic from "./icons";
import { clearConnection, getApiUrl, isConnected, login, setApiUrl } from "./api";
import { localInstalled, pathOverrides, refreshLocalAgents, setPathOverride } from "./localAgents";
import { getThemePref, setThemePref, type ThemePref } from "./theme";

const THEME_LABELS: Record<ThemePref, string> = { auto: "Auto", light: "Light", dark: "Dark" };
const THEME_PREFS: Record<string, ThemePref> = { Auto: "auto", Light: "light", Dark: "dark" };

export function Toggle(p: { on: boolean; onClick?: () => void }) {
  const [on, setOn] = createSignal(p.on);
  return (
    <button
      class={`toggle ${on() ? "on" : ""}`}
      role="switch"
      aria-checked={on()}
      onClick={() => {
        setOn(!on());
        p.onClick?.();
      }}
    />
  );
}


function Row(p: { title: string; desc?: string; children?: JSX.Element }) {
  return (
    <div class="s-row">
      <div class="s-row-text">
        <div class="s-row-title">{p.title}</div>
        <Show when={p.desc}>
          <div class="s-row-desc">{p.desc}</div>
        </Show>
      </div>
      <div class="s-row-ctrl">{p.children}</div>
    </div>
  );
}

function Card(p: { title?: string; children: JSX.Element }) {
  return (
    <section class="s-sec">
      <Show when={p.title}>
        <h3>{p.title}</h3>
      </Show>
      <div class="s-card">{p.children}</div>
    </section>
  );
}

function LocalAgentsCard() {
  const [busy, setBusy] = createSignal(false);
  const [drafts, setDrafts] = createSignal<Record<string, string>>(pathOverrides());
  const scan = async () => {
    setBusy(true);
    try {
      await refreshLocalAgents();
    } finally {
      setBusy(false);
    }
  };
  if (!localInstalled().length) void scan();
  const row = (id: string) => localInstalled().find((item) => item.id === id);
  const label: Record<string, string> = {
    claudecode: "Claude Code",
    codex: "Codex",
    grok: "Grok",
    opencode: "OpenCode",
  };
  const save = (id: string) => {
    setPathOverride(id, drafts()[id] ?? "");
    void scan();
  };
  return (
    <Card title="Local agents">
      <Row title="This computer" desc="CLIs Orb can launch here. A blank path uses whatever is on PATH.">
        <button class="s-btn" disabled={busy()} onClick={() => void scan()}>
          {busy() ? "Scanning…" : "Scan"}
        </button>
      </Row>
      <For each={["claudecode", "codex", "grok", "opencode"]}>
        {(id) => {
          const found = () => row(id);
          return (
            <Row title={label[id]} desc={found()?.installed ? `${found()?.version ?? "installed"} · ${found()?.path}` : "Not found"}>
              <input
                class="s-input"
                aria-label={`${label[id]} path`}
                placeholder="Path override"
                value={drafts()[id] ?? ""}
                onInput={(e) => setDrafts({ ...drafts(), [id]: e.currentTarget.value })}
                onKeyDown={(e) => e.key === "Enter" && save(id)}
              />
              <button class="s-btn" onClick={() => save(id)}>
                Save
              </button>
            </Row>
          );
        }}
      </For>
    </Card>
  );
}

function BackendTab() {
  const [url, setUrl] = createSignal(getApiUrl());
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const connect = async () => {
    if (busy()) return;
    setApiUrl(url());
    setBusy(true);
    setError(null);
    try {
      await login(password());
      setPassword("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Card title="Backend">
        <Row title="API URL" desc="Base URL of the sandboxed.sh core backend.">
          <input
            class="s-input"
            value={url()}
            placeholder="https://agent-backend.thomas.md"
            onInput={(e) => setUrl(e.currentTarget.value)}
          />
        </Row>
        <Show when={!isConnected()}>
          <Row title="Password" desc="Dashboard password. The returned JWT is stored in localStorage.">
            <input
              class="s-input"
              type="password"
              value={password()}
              onInput={(e) => setPassword(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && void connect()}
            />
          </Row>
        </Show>
        <Row
          title={isConnected() ? "Connected" : "Not connected"}
          desc={isConnected() ? `Signed in to ${getApiUrl()}.` : "Connect to enable live Machines, Providers and missions."}
        >
          <Show
            when={isConnected()}
            fallback={
              <button class="s-btn primary" disabled={busy() || !password()} onClick={() => void connect()}>
                {busy() ? "Connecting…" : "Connect"}
              </button>
            }
          >
            <button class="s-btn" onClick={() => clearConnection()}>
              Disconnect
            </button>
          </Show>
        </Row>
      </Card>
      <Show when={error()}>
        <ErrorNotice error={error()!} />
      </Show>
    </>
  );
}

export function Settings(p: { onOpenPage?: (id: string) => void } = {}) {
  const [executionMounted, setExecutionMounted] = createSignal(false);
  return (
    <div class="s-body settings-body">
      <div class="s-inner">
        <h2>Client</h2>
        <BackendTab />
        <LocalAgentsCard />
        <Show when={isConnected()}>
          <section class="s-sec">
            <h3>Execution</h3>
            <details class="s-card execution-disclosure" onToggle={e => { if (e.currentTarget.open) setExecutionMounted(true); }}>
              <summary class="s-row">
                <span class="s-row-text"><span class="s-row-title">Concurrency limits</span><span class="s-row-desc">Agent and task limits across all projects.</span></span>
                <Ic.ChevronDown size={14} />
              </summary>
              <Show when={executionMounted()}><ExecutionSettings inline /></Show>
            </details>
          </section>
        </Show>
        <Card title="Appearance">
          <Row title="Theme" desc="Auto follows your desktop appearance.">
            <Select
              value={THEME_LABELS[getThemePref()]}
              aria-label="Theme"
              onChange={(e) => setThemePref(THEME_PREFS[e.currentTarget.value])}
            ><option>Auto</option><option>Light</option><option>Dark</option></Select>
          </Row>
        </Card>
      </div>
    </div>
  );
}
