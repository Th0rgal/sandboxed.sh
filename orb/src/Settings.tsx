import { For, Show, createSignal, type JSX } from "solid-js";
import * as Ic from "./icons";
import { clearConnection, getApiUrl, isConnected, login, setApiUrl } from "./api";
import { getThemePref, setThemePref, type ThemePref } from "./theme";

const THEME_LABELS: Record<ThemePref, string> = { auto: "Auto", light: "Light", dark: "Dark" };
const THEME_PREFS: Record<string, ThemePref> = { Auto: "auto", Light: "light", Dark: "dark" };

export const SETTINGS_TABS = [
  { id: "backend", label: "Backend", icon: Ic.SlidersIcon },
  { id: "general", label: "General", icon: Ic.GearIcon },
  { id: "appearance", label: "Appearance", icon: Ic.AppearanceIcon },
  { id: "agents", label: "Agents", icon: Ic.NewAgentIcon },
  { id: "models", label: "Models", icon: Ic.CubeIcon },
] as const;

export type SettingsTab = (typeof SETTINGS_TABS)[number]["id"];

const MODELS = [
  { name: "Orb Lorem 4.6 High Fast", cap: "200k", on: true },
  { name: "Orb Lorem 4.6", cap: "200k", on: true },
  { name: "Ipsum 5 Max", cap: "272k", on: true },
  { name: "Ipsum 5", cap: "128k", on: false },
  { name: "Dolor 4.5 Sonnet", cap: "200k", on: true },
  { name: "Dolor 4.5 Opus", cap: "200k", on: false },
  { name: "Auto", cap: "Router", on: true },
];

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

function Select(p: { value: string; options: string[]; onChange?: (v: string) => void }) {
  const [v, setV] = createSignal(p.value);
  const [open, setOpen] = createSignal(false);
  return (
    <div class="s-select" onPointerDown={(e) => e.stopPropagation()}>
      <button class="s-btn" onClick={() => setOpen(!open())}>
        {v()} <Ic.ChevronDown size={12} />
      </button>
      <Show when={open()}>
        <div class="menu s-menu">
          <For each={p.options}>
            {(o) => (
              <button
                class={`menu-item ${o === v() ? "on" : ""}`}
                onClick={() => {
                  setV(o);
                  setOpen(false);
                  p.onChange?.(o);
                }}
              >
                {o}
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
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
      <h2>Backend</h2>
      <Card title="Sandboxed.sh core">
        <Row title="API URL" desc="Base URL of the sandboxed.sh core backend.">
          <input
            class="s-input"
            value={url()}
            placeholder="https://agent-backend.thomas.md"
            onInput={(e) => {
              setUrl(e.currentTarget.value);
              setApiUrl(e.currentTarget.value);
            }}
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
        <div class="p-toast">{error()}</div>
      </Show>
    </>
  );
}

export function Settings(p: { tab: SettingsTab }) {
  return (
    <div class="s-body">
      <div class="s-inner">
        <Show when={p.tab === "backend"}>
          <BackendTab />
        </Show>

        <Show when={p.tab === "general"}>
          <h2>General</h2>
          <Card title="Notifications">
            <Row title="System Notifications" desc="Show system notifications when an agent completes or needs attention">
              <Toggle on />
            </Row>
            <Row title="Warning Notifications" desc="Show warning-level in-app toasts">
              <Toggle on={false} />
            </Row>
            <Row title="Menu Bar Icon" desc="Show Orb in the menu bar">
              <Toggle on />
            </Row>
            <Row title="Completion Sound" desc="Play a sound when an agent finishes responding">
              <Toggle on={false} />
            </Row>
          </Card>
          <Card title="Privacy">
            <Row title="Privacy Mode" desc="Your code data will not be trained on or used to improve the product.">
              <Select value="Privacy Mode" options={["Privacy Mode", "Share Data"]} />
            </Row>
          </Card>
        </Show>

        <Show when={p.tab === "appearance"}>
          <h2>Appearance</h2>
          <Card>
            <Row title="Theme" desc="Auto follows your desktop appearance.">
              <Select
                value={THEME_LABELS[getThemePref()]}
                options={["Auto", "Light", "Dark"]}
                onChange={(v) => setThemePref(THEME_PREFS[v])}
              />
            </Row>
            <Row title="Text Size" desc="Size of the conversation transcript.">
              <Select value="Default" options={["Small", "Default", "Large"]} />
            </Row>
          </Card>
        </Show>

        <Show when={p.tab === "agents"}>
          <h2>Agents</h2>
          <Card>
            <Row title="Default Mode" desc="Mode used when you open a new agent.">
              <Select value="Agent" options={["Agent", "Plan", "Ask", "Last used mode"]} />
            </Row>
            <Row title="Queue Messages" desc="What happens if you send while an agent is working.">
              <Select value="Send after current message" options={["Send after current message", "Stop & send right away"]} />
            </Row>
          </Card>
          <Card title="Auto-Run">
            <Row title="Auto-Run Mode" desc="How freely agents may run tools without asking.">
              <Select value="Auto-Run in Sandbox" options={["Ask Every Time", "Auto-Run in Sandbox", "Run Everything"]} />
            </Row>
            <Row title="File-Deletion Protection" desc="Always ask before an agent deletes files.">
              <Toggle on />
            </Row>
            <Row title="Dotfile Protection" desc="Always ask before editing files like .gitignore.">
              <Toggle on />
            </Row>
          </Card>
        </Show>

        <Show when={p.tab === "models"}>
          <h2>Models</h2>
          <p class="s-lead">When Orb is wired to sandboxed.sh, this list will come from connected providers. Until then it is local.</p>
          <Card>
            <For each={MODELS}>
              {(m) => (
                <Row title={m.name} desc={m.cap}>
                  <Toggle on={m.on} />
                </Row>
              )}
            </For>
          </Card>
        </Show>
      </div>
    </div>
  );
}
