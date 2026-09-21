import { For, Show, createSignal, type JSX } from "solid-js";
import * as Ic from "./icons";
import { clearConnection, getApiUrl, isConnected, login, setApiUrl } from "./api";
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
        <div class="p-toast">{error()}</div>
      </Show>
    </>
  );
}

export function Settings() {
  return (
    <div class="s-body">
      <div class="s-inner">
        <h2>Settings</h2>
        <BackendTab />
        <Card title="Appearance">
          <Row title="Theme" desc="Auto follows your desktop appearance.">
            <Select
              value={THEME_LABELS[getThemePref()]}
              options={["Auto", "Light", "Dark"]}
              onChange={(v) => setThemePref(THEME_PREFS[v])}
            />
          </Row>
        </Card>
      </div>
    </div>
  );
}
