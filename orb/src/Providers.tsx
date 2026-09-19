import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { createStore, produce } from "solid-js/store";
import * as Ic from "./icons";
import { Dialog, Field } from "./Dialog";
import { Toggle } from "./Settings";
import {
  getAllProviderUsage,
  getCliProxyLogin,
  isConnected,
  listProviders,
  openExternalUrl,
  startCliProxyLogin,
  submitCliProxyLoginCallback,
  type AIProvider,
  type ProviderUsage,
} from "./api";

type AuthKind = "oauth" | "api";
type Owner = "cliproxy" | "sandboxed" | "gemini";
type Status = "connected" | "needs_reauth" | "not_configured";

type Method = { label: string; kind: AuthKind; desc: string };

type Kind = {
  id: string;
  name: string;
  methods: Method[];
};

const KINDS: Kind[] = [
  {
    id: "anthropic",
    name: "Anthropic",
    methods: [
      { label: "Claude Pro/Max", kind: "oauth", desc: "Subscription via CLIProxyAPI. sandboxed.sh does not refresh this token." },
      { label: "API key", kind: "api", desc: "Direct Anthropic API key, stored by sandboxed.sh." },
    ],
  },
  {
    id: "openai",
    name: "OpenAI",
    methods: [
      { label: "ChatGPT Plus/Pro", kind: "oauth", desc: "Codex / ChatGPT plan via CLIProxyAPI." },
      { label: "API key", kind: "api", desc: "api.openai.com key. Separate from the ChatGPT plan." },
    ],
  },
  {
    id: "xai",
    name: "xAI",
    methods: [
      { label: "SuperGrok OAuth", kind: "oauth", desc: "grok.com login via CLIProxyAPI. Grok Build shares this account." },
      { label: "API key", kind: "api", desc: "Direct xAI API key." },
    ],
  },
  {
    id: "google",
    name: "Google",
    methods: [
      { label: "Gemini CLI OAuth", kind: "oauth", desc: "Own oauth_creds.json. Not owned by CLIProxyAPI." },
      { label: "API key", kind: "api", desc: "Google AI Studio key." },
    ],
  },
  {
    id: "kimi",
    name: "Kimi",
    methods: [{ label: "Kimi Code", kind: "oauth", desc: "Device OAuth. Still owned by sandboxed.sh (300s tokens)." }],
  },
  {
    id: "github-copilot",
    name: "GitHub Copilot",
    methods: [{ label: "GitHub Copilot", kind: "oauth", desc: "OAuth subscription." }],
  },
  { id: "open-router", name: "OpenRouter", methods: [{ label: "API key", kind: "api", desc: "OPENROUTER_API_KEY" }] },
  { id: "groq", name: "Groq", methods: [{ label: "API key", kind: "api", desc: "GROQ_API_KEY" }] },
  { id: "mistral", name: "Mistral", methods: [{ label: "API key", kind: "api", desc: "MISTRAL_API_KEY" }] },
  { id: "minimax", name: "MiniMax", methods: [{ label: "API key", kind: "api", desc: "MINIMAX_API_KEY" }] },
  { id: "zai", name: "Z.AI", methods: [{ label: "API key", kind: "api", desc: "ZHIPU_API_KEY" }] },
  { id: "custom", name: "Custom", methods: [{ label: "OpenAI-compatible", kind: "api", desc: "Base URL + key." }] },
];

const BACKENDS: Record<string, string[]> = {
  anthropic: ["claudecode", "opencode"],
  openai: ["codex", "opencode"],
  xai: ["grok", "opencode"],
  google: ["gemini", "opencode"],
  kimi: ["opencode"],
  "github-copilot": ["opencode"],
};

const BACKEND_LABEL: Record<string, string> = {
  claudecode: "Claude Code",
  opencode: "OpenCode",
  codex: "Codex",
  grok: "Grok",
  gemini: "Gemini",
};

type Account = {
  id: string;
  type: string;
  name: string;
  method: string;
  auth: AuthKind;
  owner: Owner;
  status: Status;
  email?: string;
  backends: string[];
  enabled: boolean;
};

function ownerFor(type: string, auth: AuthKind): Owner {
  if (auth === "api") return "sandboxed";
  if (type === "google") return "gemini";
  if (type === "kimi" || type === "github-copilot") return "sandboxed";
  if (type === "anthropic" || type === "openai" || type === "xai") return "cliproxy";
  return "sandboxed";
}

function ownerLabel(o: Owner) {
  if (o === "cliproxy") return "CLIProxyAPI";
  if (o === "gemini") return "Gemini CLI";
  return "sandboxed.sh";
}

const SEED: Account[] = [
  {
    id: "a-claude",
    type: "anthropic",
    name: "Anthropic",
    method: "Claude Pro/Max",
    auth: "oauth",
    owner: "cliproxy",
    status: "connected",
    email: "claude.max",
    backends: ["claudecode", "opencode"],
    enabled: true,
  },
  {
    id: "a-gpt",
    type: "openai",
    name: "OpenAI",
    method: "ChatGPT Plus/Pro",
    auth: "oauth",
    owner: "cliproxy",
    status: "connected",
    email: "chatgpt.plus",
    backends: ["codex", "opencode"],
    enabled: true,
  },
  {
    id: "a-xai",
    type: "xai",
    name: "xAI",
    method: "SuperGrok OAuth",
    auth: "oauth",
    owner: "cliproxy",
    status: "connected",
    email: "grok.build",
    backends: ["grok", "opencode"],
    enabled: true,
  },
  {
    id: "a-gem",
    type: "google",
    name: "Google",
    method: "Gemini CLI OAuth",
    auth: "oauth",
    owner: "gemini",
    status: "connected",
    backends: ["gemini", "opencode"],
    enabled: true,
  },
  {
    id: "a-or",
    type: "open-router",
    name: "OpenRouter",
    method: "API key",
    auth: "api",
    owner: "sandboxed",
    status: "connected",
    backends: ["opencode"],
    enabled: true,
  },
];

export function Providers() {
  const [list, setList] = createStore<Account[]>(SEED.map((a) => ({ ...a })));
  const [remote, setRemote] = createSignal<AIProvider[] | null>(null);
  const [add, setAdd] = createSignal(false);

  const reloadRemote = () => {
    if (isConnected()) {
      listProviders()
        .then(setRemote)
        .catch(() => {});
    }
  };
  onMount(reloadRemote);

  const live = () => (isConnected() ? remote() : null);
  const [step, setStep] = createSignal<"type" | "method" | "details">("type");
  const [kindId, setKindId] = createSignal<string | null>(null);
  const [methodI, setMethodI] = createSignal(0);
  const [key, setKey] = createSignal("");
  const [backends, setBackends] = createSignal<string[]>([]);
  const [toast, setToast] = createSignal<string | null>(null);

  const kind = createMemo(() => KINDS.find((k) => k.id === kindId()) ?? null);
  const method = createMemo(() => kind()?.methods[methodI()] ?? null);
  const oauth = () => list.filter((a) => a.auth === "oauth");
  const keys = () => list.filter((a) => a.auth === "api");

  const resetAdd = () => {
    setAdd(false);
    setStep("type");
    setKindId(null);
    setMethodI(0);
    setKey("");
    setBackends([]);
  };

  const pickType = (id: string) => {
    setKindId(id);
    setMethodI(0);
    setBackends(BACKENDS[id] ?? ["opencode"]);
    const k = KINDS.find((x) => x.id === id);
    if (k && k.methods.length === 1) setStep("details");
    else setStep("method");
  };

  const finish = () => {
    const k = kind();
    const m = method();
    if (!k || !m) return;
    if (m.kind === "api" && !key().trim()) return;
    setList(list.length, {
      id: "p" + Date.now(),
      type: k.id,
      name: k.name,
      method: m.label,
      auth: m.kind,
      owner: ownerFor(k.id, m.kind),
      status: "connected",
      backends: backends(),
      enabled: true,
    });
    setToast(m.kind === "oauth" ? "Would open the sandboxed.sh OAuth flow. Not connected yet." : "Would store this key in sandboxed.sh. Not saved yet.");
    setTimeout(() => setToast(null), 2800);
    resetAdd();
  };

  const signIn = (a: Account) => {
    setToast(`Would start ${a.method} via ${ownerLabel(a.owner)}. Not connected yet.`);
    setTimeout(() => setToast(null), 2800);
  };

  return (
    <Show when={!live()} fallback={<LiveProviders list={live() ?? []} onRefresh={reloadRemote} />}>
    <div class="page">
      <div class="page-head">
        <h2>Providers</h2>
        <button class="s-btn" onClick={() => setAdd(true)}>
          <Ic.PlusIcon size={14} /> Add
        </button>
      </div>
      <p class="s-lead">
        Accounts sync from sandboxed.sh. Claude, ChatGPT and xAI OAuth are owned by CLIProxyAPI — Orb will not refresh those tokens.
      </p>

      <Show when={toast()}>
        <div class="p-toast">{toast()}</div>
      </Show>

      <section class="s-sec">
        <h3>Subscriptions</h3>
        <div class="s-card">
          <For each={oauth()}>
            {(a) => (
              <AccountRow
                a={a}
                onSignIn={() => signIn(a)}
                onToggle={() => setList(list.findIndex((x) => x.id === a.id), "enabled", !a.enabled)}
                onRemove={() =>
                  setList(
                    produce((ls) => {
                      const j = ls.findIndex((x) => x.id === a.id);
                      if (j >= 0) ls.splice(j, 1);
                    }),
                  )
                }
              />
            )}
          </For>
          <Show when={oauth().length === 0}>
            <div class="s-row"><div class="s-row-desc">No OAuth accounts yet.</div></div>
          </Show>
        </div>
      </section>

      <section class="s-sec">
        <h3>API keys</h3>
        <div class="s-card">
          <For each={keys()}>
            {(a) => (
              <AccountRow
                a={a}
                onSignIn={() => signIn(a)}
                onToggle={() => setList(list.findIndex((x) => x.id === a.id), "enabled", !a.enabled)}
                onRemove={() =>
                  setList(
                    produce((ls) => {
                      const j = ls.findIndex((x) => x.id === a.id);
                      if (j >= 0) ls.splice(j, 1);
                    }),
                  )
                }
              />
            )}
          </For>
          <Show when={keys().length === 0}>
            <div class="s-row"><div class="s-row-desc">No API keys yet.</div></div>
          </Show>
        </div>
      </section>

      <Show when={add()}>
        <Dialog
          title={step() === "type" ? "Add provider" : step() === "method" ? kind()?.name ?? "Method" : method()?.label ?? "Details"}
          onClose={resetAdd}
          footer={
            <>
              <Show when={step() !== "type"}>
                <button class="s-btn" onClick={() => setStep(step() === "details" && (kind()?.methods.length ?? 0) > 1 ? "method" : "type")}>
                  Back
                </button>
              </Show>
              <span class="dlg-spacer" />
              <button class="s-btn" onClick={resetAdd}>Cancel</button>
              <Show when={step() === "details"}>
                <button class="s-btn primary" onClick={finish}>
                  {method()?.kind === "oauth" ? "Sign in" : "Save"}
                </button>
              </Show>
            </>
          }
        >
          <Show when={step() === "type"}>
            <div class="p-grid">
              <For each={KINDS}>
                {(k) => (
                  <button class="p-type" onClick={() => pickType(k.id)}>
                    <div class="p-type-name">{k.name}</div>
                    <div class="p-type-meta">{k.methods.some((m) => m.kind === "oauth") ? "OAuth" : "API key"}</div>
                  </button>
                )}
              </For>
            </div>
          </Show>
          <Show when={step() === "method"}>
            <For each={kind()?.methods ?? []}>
              {(m, i) => (
                <button class="p-method" onClick={() => { setMethodI(i()); setStep("details"); }}>
                  <div class="s-row-title">{m.label}</div>
                  <div class="s-row-desc">{m.desc}</div>
                </button>
              )}
            </For>
          </Show>
          <Show when={step() === "details"}>
            <p class="s-lead" style={{ "margin-bottom": "12px" }}>{method()?.desc}</p>
            <Show when={(BACKENDS[kindId() ?? ""] ?? []).length > 0}>
              <div class="field">
                <span>Use for</span>
                <div class="p-backs">
                  <For each={BACKENDS[kindId() ?? ""] ?? ["opencode"]}>
                    {(b) => (
                      <button
                        class={`pill ${backends().includes(b) ? "on" : ""}`}
                        onClick={() =>
                          setBackends(backends().includes(b) ? backends().filter((x) => x !== b) : [...backends(), b])
                        }
                      >
                        {BACKEND_LABEL[b] ?? b}
                      </button>
                    )}
                  </For>
                </div>
              </div>
            </Show>
            <Show when={method()?.kind === "api"}>
              <Field label="API key">
                <input type="password" placeholder="sk-…" value={key()} onInput={(e) => setKey(e.currentTarget.value)} />
              </Field>
            </Show>
            <Show when={method()?.kind === "oauth"}>
              <p class="s-row-desc">Sign-in will be handed to sandboxed.sh / CLIProxyAPI. Nothing is stored in Orb yet.</p>
            </Show>
          </Show>
        </Dialog>
      </Show>
    </div>
    </Show>
  );
}

function LiveProviders(p: { list: AIProvider[]; onRefresh: () => void }) {
  const [usage, setUsage] = createSignal<Record<string, ProviderUsage>>({});
  const [reauth, setReauth] = createSignal<AIProvider | null>(null);
  const oauth = () => p.list.filter((x) => x.uses_oauth);
  const keys = () => p.list.filter((x) => !x.uses_oauth);

  const refreshUsage = () =>
    getAllProviderUsage()
      .then(setUsage)
      .catch(() => {});
  onMount(refreshUsage);

  return (
    <div class="page">
      <div class="page-head">
        <h2>Providers</h2>
        <button class="s-btn" onClick={() => { refreshUsage(); p.onRefresh(); }}>
          Refresh
        </button>
      </div>
      <p class="s-lead">Configured providers from the connected sandboxed.sh backend.</p>

      <section class="s-sec">
        <h3>Subscriptions (OAuth)</h3>
        <div class="s-card">
          <For each={oauth()}>
            {(a) => <LiveRow a={a} usage={usage()[a.id]} onReconnect={() => setReauth(a)} />}
          </For>
          <Show when={oauth().length === 0}>
            <div class="s-row"><div class="s-row-desc">No OAuth providers configured.</div></div>
          </Show>
        </div>
      </section>

      <section class="s-sec">
        <h3>API keys</h3>
        <div class="s-card">
          <For each={keys()}>
            {(a) => <LiveRow a={a} usage={usage()[a.id]} onReconnect={() => setReauth(a)} />}
          </For>
          <Show when={keys().length === 0}>
            <div class="s-row"><div class="s-row-desc">No API key providers configured.</div></div>
          </Show>
        </div>
      </section>

      <Show when={reauth()}>
        {(a) => (
          <ReAuthDialog
            provider={a()}
            onClose={() => setReauth(null)}
            onDone={() => {
              setReauth(null);
              p.onRefresh();
              refreshUsage();
            }}
          />
        )}
      </Show>
    </div>
  );
}

const CLIPROXY_LOGIN_TYPES = new Set(["anthropic", "openai", "xai", "kimi"]);

/** Reconnect via CLIProxyAPI when the backend says so; fall back to the
 * type allowlist for backends that predate the credential_owner field. */
function cliProxyReconnectable(a: AIProvider): boolean {
  if (!a.uses_oauth) return false;
  if (a.credential_owner) return a.credential_owner === "cli_proxy";
  return CLIPROXY_LOGIN_TYPES.has(a.provider_type);
}

function UsageBars(p: { usage: ProviderUsage }) {
  const u = p.usage;
  const windows = createMemo(() => {
    const out: { label: string; used: number }[] = [];
    if (u.unified_5h_utilization != null) out.push({ label: "5h", used: u.unified_5h_utilization });
    if (u.unified_7d_utilization != null) out.push({ label: "7d", used: u.unified_7d_utilization });
    return out;
  });
  return (
    <Show when={windows().length > 0}>
      <div class="p-usage">
        <For each={windows()}>
          {(w) => (
            <div class="p-usage-row">
              <span class="p-usage-label">{w.label}</span>
              <div class="p-bar">
                <div
                  class={`p-bar-fill ${w.used > 0.9 ? "hot" : w.used > 0.7 ? "warm" : ""}`}
                  style={{ width: `${Math.min(100, Math.round(w.used * 100))}%` }}
                />
              </div>
              <span class="p-usage-pct">{Math.round(w.used * 100)}%</span>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

function ReAuthDialog(p: { provider: AIProvider; onClose: () => void; onDone: () => void }) {
  const [session, setSession] = createSignal<{ id: string; url: string } | null>(null);
  const [phase, setPhase] = createSignal<"starting" | "awaiting" | "finishing" | "failed">("starting");
  const [error, setError] = createSignal<string | null>(null);
  const [paste, setPaste] = createSignal("");
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  const stopPolling = () => {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = undefined;
  };
  onCleanup(stopPolling);

  const startPolling = (id: string) => {
    stopPolling();
    pollTimer = setInterval(() => {
      getCliProxyLogin(id)
        .then((st) => {
          if (st.status === "completed") {
            stopPolling();
            p.onDone();
          } else if (st.status === "failed") {
            stopPolling();
            setPhase("failed");
            setError(st.message ?? "login failed");
          }
        })
        .catch((e: Error) => {
          stopPolling();
          setPhase("failed");
          setError(e.message);
        });
    }, 2000);
  };

  onMount(() => {
    startCliProxyLogin(p.provider.provider_type)
      .then((s) => {
        setSession({ id: s.session_id, url: s.auth_url });
        setPhase("awaiting");
        startPolling(s.session_id);
        void openExternalUrl(s.auth_url);
      })
      .catch((e: Error) => {
        setPhase("failed");
        setError(e.message);
      });
  });

  const submitPaste = () => {
    const s = session();
    const url = paste().trim();
    if (!s || !url) return;
    setPhase("finishing");
    setError(null);
    submitCliProxyLoginCallback(s.id, url)
      .then((st) => {
        if (st.status === "failed") {
          setPhase("failed");
          setError(st.message ?? "callback rejected");
        } else {
          setPhase("awaiting");
        }
      })
      .catch((e: Error) => {
        setPhase("failed");
        setError(e.message);
      });
  };

  return (
    <Dialog
      title={`Reconnect ${p.provider.name}`}
      onClose={p.onClose}
      footer={
        <>
          <span class="dlg-spacer" />
          <button class="s-btn" onClick={p.onClose}>
            {phase() === "failed" ? "Close" : "Cancel"}
          </button>
        </>
      }
    >
      <Show when={phase() === "starting"}>
        <p class="s-lead">Starting the CLIProxyAPI login on the server…</p>
      </Show>
      <Show when={phase() === "failed"}>
        <p class="s-lead">Could not start the login flow.</p>
        <p class="s-row-desc">{error()}</p>
        <Show when={/^(404|405)\b/.test(error() ?? "")}>
          <p class="s-row-desc">This backend build does not expose the login endpoints yet — deploy the updated sandboxed.sh first.</p>
        </Show>
      </Show>
      <Show when={phase() === "awaiting" || phase() === "finishing"}>
        <p class="s-lead">
          Authorize in the browser window that just opened. The redirect to localhost will fail — copy the full URL from the
          address bar and paste it here.
        </p>
        <div class="field">
          <span>Auth URL</span>
          <div class="p-url">
            <code>{session()?.url}</code>
            <button class="s-btn" onClick={() => session() && void openExternalUrl(session()!.url)}>
              Open
            </button>
          </div>
        </div>
        <Field label="Redirect URL (http://localhost:…)">
          <input
            type="text"
            placeholder="http://localhost:54545/callback?code=…&state=…"
            value={paste()}
            onInput={(e) => setPaste(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && submitPaste()}
          />
        </Field>
        <Show when={error()}>
          <p class="s-row-desc">{error()}</p>
        </Show>
        <div style={{ "margin-top": "10px", display: "flex", "justify-content": "flex-end" }}>
          <button class="s-btn primary" disabled={phase() === "finishing" || !paste().trim()} onClick={submitPaste}>
            {phase() === "finishing" ? "Submitting…" : "Submit callback"}
          </button>
        </div>
      </Show>
    </Dialog>
  );
}

function LiveRow(p: { a: AIProvider; usage?: ProviderUsage; onReconnect: () => void }) {
  const a = p.a;
  const stClass = () =>
    a.status.type === "connected" ? "connected" : a.status.type === "needs_reauth" || a.status.type === "error" ? "needs_reauth" : "not_configured";
  const stLabel = () =>
    a.status.type === "connected"
      ? "Connected"
      : a.status.type === "needs_reauth"
        ? "Reconnect"
        : a.status.type === "needs_auth"
          ? "Needs auth"
          : a.status.type === "error"
            ? "Error"
            : "Unknown";
  const canCliProxyLogin = () => cliProxyReconnectable(a);
  return (
    <div class="s-row p-acc">
      <div class="s-row-text">
        <div class="s-row-title">
          {a.name}
          <span class="p-chip">{a.provider_type}</span>
        </div>
        <div class="s-row-desc">
          <span class={`p-st ${stClass()}`}>{stLabel()}</span>
          <Show when={a.account_email}>
            <span class="p-dot">·</span>
            {a.account_email}
          </Show>
        </div>
        <Show when={p.usage && !p.usage!.error}>
          <UsageBars usage={p.usage!} />
        </Show>
      </div>
      <Show when={canCliProxyLogin() && stClass() !== "connected"}>
        <button class="s-btn" onClick={p.onReconnect}>Reconnect</button>
      </Show>
    </div>
  );
}

function AccountRow(p: { a: Account; onSignIn: () => void; onToggle: () => void; onRemove: () => void }) {
  const a = p.a;
  const st = a.status === "connected" ? "Connected" : a.status === "needs_reauth" ? "Reconnect" : "Not configured";
  return (
    <div class="s-row p-acc">
      <div class="s-row-text">
        <div class="s-row-title">
          {a.name}
          <span class="p-chip">{a.method}</span>
        </div>
        <div class="s-row-desc">
          <span class={`p-st ${a.status}`}>{st}</span>
          <span class="p-dot">·</span>
          {ownerLabel(a.owner)}
          <Show when={a.backends.length}>
            <span class="p-dot">·</span>
            {a.backends.map((b) => BACKEND_LABEL[b] ?? b).join(", ")}
          </Show>
        </div>
      </div>
      <Show when={a.status !== "connected"}>
        <button class="s-btn" onClick={p.onSignIn}>{a.auth === "oauth" ? "Sign in" : "Add key"}</button>
      </Show>
      <Show when={a.status === "connected" && a.auth === "oauth"}>
        <button class="s-btn" onClick={p.onSignIn}>Reconnect</button>
      </Show>
      <Toggle on={a.enabled} onClick={p.onToggle} />
      <button class="icon-btn" title="Remove" onClick={p.onRemove}>
        <Ic.CloseIcon size={14} />
      </button>
    </div>
  );
}
