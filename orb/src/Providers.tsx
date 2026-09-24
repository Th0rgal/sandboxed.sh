import { Dynamic } from "solid-js/web";
import { codexWindowLabel, effectiveProviderStatus, hasProviderUsageDetails, usageWindows } from "./providerUsage";
import { ProviderLogo } from "./ProviderLogo";
import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { createStore, produce } from "solid-js/store";
import * as Ic from "./icons";
import { Dialog, DialogButton, Field } from "./Dialog";
import { Toggle } from "./Settings";
import {
  getAllProviderUsage,
  getProviderUsage,
  getCliProxyLogin,
  startProviderOAuth,
  completeProviderOAuth,
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
    <Show
      when={isConnected()}
      fallback={
        <div class="page">
          <div class="page-head"><h2>Providers</h2></div>
          <p class="s-lead">Providers are configured on the sandboxed.sh core — connect in Settings → Backend to see them.</p>
        </div>
      }
    >
      <Show when={live()} fallback={<div class="page"><p class="s-lead shimmer">Loading providers…</p></div>}>
        <LiveProviders list={live() ?? []} onRefresh={reloadRemote} />
      </Show>
    </Show>
  );
}


function LiveProviders(p: { list: AIProvider[]; onRefresh: () => void }) {
  const [usage, setUsage] = createSignal<Record<string, ProviderUsage>>({});
  const [reauth, setReauth] = createSignal<AIProvider | null>(null);
  const oauth = () => p.list.filter((x) => x.uses_oauth);
  const keys = () => p.list.filter((x) => !x.uses_oauth);

  let disposed = false;
  let fetching = false;
  const refreshUsage = async () => {
    if (fetching) return;
    fetching = true;
    try {
      const snapshot = await getAllProviderUsage();
      if (!disposed) setUsage(snapshot);
      // The bulk endpoint returns its cache before starting background probes.
      // Await each Codex account so the first visit receives the result too.
      await Promise.allSettled(oauth().filter(a => a.provider_type === "openai").map(async a => {
        const value = await getProviderUsage(a.id);
        if (!disposed) setUsage(previous => ({ ...previous, [a.id]: value }));
      }));
    } finally { fetching = false; }
  };
  onMount(() => {
    void refreshUsage().catch(() => {});
    const timer = setInterval(() => { void refreshUsage().catch(() => {}); }, 30_000);
    onCleanup(() => { disposed = true; clearInterval(timer); });
  });

  return (
    <div class="page">
      <div class="page-head">
        <h2>Providers</h2>
        <button class="s-btn" onClick={() => { void refreshUsage().catch(() => {}); p.onRefresh(); }}>
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
              const id = a().id;
              setReauth(null);
              setUsage(previous => { const next = { ...previous }; delete next[id]; return next; });
              p.onRefresh();
              void getProviderUsage(id, true).then(value => {
                if (!disposed) setUsage(previous => ({ ...previous, [id]: value }));
              }).catch(() => {});
            }}
          />
        )}
      </Show>
    </div>
  );
}

const LEGACY_OAUTH_TYPES = new Set(["anthropic", "openai", "google"]);
const reconnectable = (a: AIProvider) => cliProxyReconnectable(a) || (a.uses_oauth && a.credential_owner === "sandboxed_sh" && LEGACY_OAUTH_TYPES.has(a.provider_type));

const CLIPROXY_LOGIN_TYPES = new Set(["anthropic", "openai", "xai", "kimi"]);

/** Reconnect via CLIProxyAPI when the backend says so; fall back to the
 * type allowlist for backends that predate the credential_owner field. */
function cliProxyReconnectable(a: AIProvider): boolean {
  if (!a.uses_oauth) return false;
  if (a.credential_owner) return a.credential_owner === "cli_proxy";
  return CLIPROXY_LOGIN_TYPES.has(a.provider_type);
}

function UsageSummary(p: { usage: ProviderUsage }) {
  const windows = createMemo(() => usageWindows(p.usage));
  // Keep the account summary compact; detailed meters live in the expansion.
  return (
    <Show when={windows().length > 0}>
      <div class="p-usage compact">
        <For each={windows()}>
          {(w) => (
            <span class="p-usage-chip" title={`${w.label} window: ${Math.round(w.used * 100)}% used`}>
              <span class="p-usage-label">{w.label}</span>
              <span class="p-usage-pct">{Math.round(w.used * 100)}%</span>
            </span>
          )}
        </For>
      </div>
    </Show>
  );
}

function ReAuthDialog(p: { provider: AIProvider; onClose: () => void; onDone: () => void }) {
  const [session, setSession] = createSignal<{ id: string; url: string; flow?: string; instructions?: string } | null>(null);
  const proxy = cliProxyReconnectable(p.provider);
  let disposed = false;
  const [phase, setPhase] = createSignal<"starting" | "awaiting" | "finishing" | "failed">("starting");
  const [error, setError] = createSignal<string | null>(null);
  const [paste, setPaste] = createSignal("");
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  const stopPolling = () => {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = undefined;
  };
  onCleanup(() => { disposed = true; stopPolling(); });

  const startPolling = (id: string) => {
    stopPolling();
    pollTimer = setInterval(() => {
      getCliProxyLogin(id)
        .then((st) => {
          if (disposed) return;
          if (st.status === "completed") {
            stopPolling();
            p.onDone();
          } else if (st.status === "failed") {
            stopPolling();
            setError(st.message ?? "login failed");
            setPhase("failed");
          }
        })
        .catch((e: Error) => {
          stopPolling();
          setError(e.message);
          setPhase("failed");
        });
    }, 2000);
  };

  onMount(() => {
    const start = proxy
      ? startCliProxyLogin(p.provider.provider_type).then(s => ({ id: s.session_id, url: s.auth_url, flow: s.flow, instructions: undefined as string | undefined }))
      : startProviderOAuth(p.provider.id).then(s => ({ id: p.provider.id, url: s.url, flow: s.method, instructions: s.instructions }));
    start
      .then((s) => {
        if (disposed) return;
        setSession(s);
        setPhase("awaiting");
        if (proxy) startPolling(s.id);
        void openExternalUrl(s.url).catch(e => setError(String(e)));
      })
      .catch((e: Error) => {
        setError(e.message);
        setPhase("failed");
      });
  });

  const submitPaste = () => {
    const s = session();
    const url = paste().trim();
    if (!s || !url || phase() === "finishing") return;
    setPhase("finishing");
    setError(null);
    const submit = proxy ? submitCliProxyLoginCallback(s.id, url)
      : completeProviderOAuth(p.provider.id, url).then(() => ({ status: "completed" as const, message: undefined }));
    submit.then((st) => {
        if (disposed) return;
        if (st.status === "completed") { stopPolling(); p.onDone(); return; }
        if (st.status === "failed") {
          setError(st.message ?? "callback rejected");
          setPhase("failed");
        } else {
          setPhase("awaiting");
        }
      })
      .catch((e: Error) => {
        setError(e.message);
        setPhase("failed");
      });
  };

  return (
    <Dialog
      title={`Reconnect ${p.provider.name}`}
      busy={phase() === "finishing"}
      onClose={p.onClose}
      footer={
        <>
          <DialogButton disabled={phase() === "finishing"} onClick={p.onClose}>
            {phase() === "failed" ? "Close login" : "Cancel"}
          </DialogButton>
        </>
      }
    >
      <Show when={phase() === "starting"}>
        <p class="s-lead">Starting the account login…</p>
      </Show>
      <Show when={phase() === "failed"}>
        <p class="s-lead">Could not complete the login.</p>
        <ErrorNotice error={error()!} />
        <Show when={/^(404|405)\b/.test(error() ?? "")}>
          <p class="s-row-desc">This backend build does not expose the login endpoints yet — deploy the updated sandboxed.sh first.</p>
        </Show>
      </Show>
      <Show when={phase() === "awaiting" || phase() === "finishing"}>
        <p class="s-lead">
          {session()?.instructions ?? (session()?.flow === "device"
            ? "Authorize in the browser window that just opened and enter the code shown — the login completes automatically."
            : "Authorize in the browser window that just opened. The redirect to localhost will fail — copy the full URL from the address bar and paste it here.")}
        </p>
        <div class="field">
          <Show when={p.provider.account_email}><p>Sign in as {p.provider.account_email} to reconnect this account.</p></Show>
          <span>Auth URL</span>
          <div class="p-url">
            <code>{session()?.url}</code>
            <DialogButton onClick={() => session() && void openExternalUrl(session()!.url)}>
              Open
            </DialogButton>
          </div>
        </div>
        <Show when={session()?.flow !== "device"}>
          <Field label={proxy ? "Redirect URL (http://localhost:…)" : "Authorization code or redirect URL"}>
            <input
              type="text"
              placeholder={proxy ? "http://localhost:54545/callback?code=…&state=…" : "Paste the code or full redirect URL"}
              value={paste()}
              onInput={(e) => setPaste(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && submitPaste()}
            />
          </Field>
        </Show>
        <Show when={error()}>
          <ErrorNotice error={error()!} />
        </Show>
        <Show when={session()?.flow !== "device"}>
          <div class="p-acc-actions">
            <DialogButton variant="primary" disabled={phase() === "finishing" || !paste().trim()} onClick={submitPaste}>
              {phase() === "finishing" ? "Submitting…" : "Submit callback"}
            </DialogButton>
          </div>
        </Show>
      </Show>
    </Dialog>
  );
}

/** ISO timestamp, epoch-seconds string, or relative ("2s") → readable label. */
function fmtReset(v: string): string {
  if (/^\d+$/.test(v)) return fmtResetEpoch(v.length > 12 ? Number(v) / 1000 : Number(v));
  const t = Date.parse(v);
  if (Number.isNaN(t)) return v;
  return fmtResetEpoch(t / 1000);
}

function fmtResetEpoch(sec: number): string {
  const delta = sec * 1000 - Date.now();
  if (delta <= 0) return "now";
  const mins = Math.round(delta / 60000);
  if (mins < 60) return `in ${mins}m`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `in ${hours}h`;
  return `in ${Math.round(hours / 24)}d`;
}

function DetailBar(p: { label: string; usedPct: number; reset?: string }) {
  const pct = () => Math.max(0, Math.min(100, Math.round(p.usedPct)));
  return (
    <div class="p-usage-grid">
      <span class="p-usage-label p-usage-label-wide">{p.label}</span>
      <div class="p-bar">
        <div class={`p-bar-fill ${pct() > 90 ? "hot" : pct() > 70 ? "warm" : ""}`} style={{ width: `${pct()}%` }} />
      </div>
      <span class="p-usage-pct">{pct()}%</span>
      <span class="p-usage-reset">{p.reset ?? ""}</span>
    </div>
  );
}

function UsageDetail(p: { usage: ProviderUsage; headerEmail?: string; planInHeader?: boolean }) {
  const u = () => p.usage;
  const type = () => u().provider_type;
  return (
    <div class="p-detail">
      <Show when={u().status === "needs_reauth"}><p class="s-row-desc c-red">Reconnect this account to check its quota and use it for new requests.</p></Show>
      <div class="p-detail-meta">
        <Show when={u().account_email && u().account_email !== p.headerEmail}><span>{u().account_email}</span></Show>
        <Show when={u().account_name}><span>{u().account_name}</span></Show>
        <Show when={u().organization}><span>{u().organization}</span></Show>
        <Show when={u().unified_status}>
          <span class={u().unified_status === "ok" || u().unified_status === "allowed" ? "c-green" : "c-red"}>status: {u().unified_status}</span>
        </Show>
      </div>

      <Show when={type() === "anthropic" && u().unified_5h_utilization != null}>
        <DetailBar label="5h window" usedPct={(u().unified_5h_utilization ?? 0) * 100} reset={u().unified_5h_reset ? `reset ${fmtReset(u().unified_5h_reset!)}` : undefined} />
      </Show>
      <Show when={type() === "anthropic" && u().unified_7d_utilization != null}>
        <DetailBar label="7d window" usedPct={(u().unified_7d_utilization ?? 0) * 100} reset={u().unified_7d_reset ? `reset ${fmtReset(u().unified_7d_reset!)}` : undefined} />
      </Show>

      <Show when={type() === "openai" && u().codex_primary_used_percent != null && u().codex_primary_window_minutes !== 0}>
        <Show when={u().codex_plan_type && !p.planInHeader}>
          <div class="p-detail-meta"><span>plan: {u().codex_plan_type}</span></div>
        </Show>
        <DetailBar label={codexWindowLabel(u().codex_primary_window_minutes, "Primary window")} usedPct={u().codex_primary_used_percent ?? 0} reset={u().codex_primary_reset_at ? `reset ${fmtResetEpoch(u().codex_primary_reset_at!)}` : undefined} />
        <Show when={u().codex_secondary_used_percent != null && u().codex_secondary_window_minutes !== 0}>
          <DetailBar label={codexWindowLabel(u().codex_secondary_window_minutes, "Secondary window")} usedPct={u().codex_secondary_used_percent ?? 0} reset={u().codex_secondary_reset_at ? `reset ${fmtResetEpoch(u().codex_secondary_reset_at!)}` : undefined} />
        </Show>
      </Show>
      <Show when={type() === "openai" && u().requests_limit != null}>
        <DetailBar label="Requests" usedPct={100 - ((u().requests_remaining ?? 0) / (u().requests_limit ?? 1)) * 100} reset={u().requests_reset ? `reset ${fmtReset(u().requests_reset!)}` : undefined} />
      </Show>

      <Show when={type() === "minimax" && u().model_usage && (u().model_usage?.length ?? 0) > 0}>
        <For each={u().model_usage}>
          {(m) => (
            <div class="p-model">
              <div class="p-model-name">{m.model}</div>
              <DetailBar label="5h window" usedPct={100 - m.interval_remaining_percent} reset={m.interval_reset > 0 ? `reset ${fmtResetEpoch(m.interval_reset)}` : undefined} />
              <DetailBar label="Weekly" usedPct={100 - m.weekly_remaining_percent} reset={m.weekly_reset > 0 ? `reset ${fmtResetEpoch(m.weekly_reset)}` : undefined} />
            </div>
          )}
        </For>
      </Show>

      <Show when={type() === "zai" && u().zai_tokens_percentage != null}>
        <Show when={u().zai_plan}>
          <div class="p-detail-meta"><span>plan: {u().zai_plan}</span></div>
        </Show>
        <DetailBar label="Tokens" usedPct={u().zai_tokens_percentage ?? 0} reset={u().zai_tokens_reset ? `reset ${fmtResetEpoch(u().zai_tokens_reset!)}` : undefined} />
        <Show when={u().zai_mcp_percentage != null}>
          <DetailBar label="MCP" usedPct={u().zai_mcp_percentage ?? 0} reset={u().zai_mcp_reset ? `reset ${fmtResetEpoch(u().zai_mcp_reset!)}` : undefined} />
        </Show>
      </Show>

      <Show when={u().error}>
        <p class="s-row-desc c-red">{u().error}</p>
      </Show>
    </div>
  );
}

function LiveRow(p: { a: AIProvider; usage?: ProviderUsage; onReconnect: () => void }) {
  const a = p.a;
  const status = () => effectiveProviderStatus(a, p.usage);
  const stClass = () => status() === "connected" ? "connected" : ["needs_reauth", "error", "quota_exhausted"].includes(status()) ? "needs_reauth" : "not_configured";
  const stLabel = () => ({ connected: "Connected", needs_reauth: "Reconnect", quota_exhausted: "Quota exhausted", needs_auth: "Needs auth", error: "Error" }[status()] ?? "Unknown");
  const canReconnect = () => reconnectable(a);
  const expandable = () => canReconnect() || hasProviderUsageDetails(p.usage) || !!a.status.reason || !!a.status.message;
  const [open, setOpen] = createSignal(false);
  return (
    <div class="p-acc-wrap">
      <Dynamic component={expandable() ? "button" : "div"} class={`s-row p-acc ${expandable() ? "p-acc-btn" : ""}`} aria-expanded={expandable() ? open() : undefined} onClick={expandable() ? () => setOpen(!open()) : undefined}>
        <ProviderLogo type={a.provider_type} name={a.name} />
        <div class="s-row-text">
          <div class="s-row-title">
            {a.name}
            <span class="p-chip">{a.provider_type}</span>
          </div>
          <div class="s-row-desc">
            <span class={`p-st ${stClass()}`}>{stLabel()}</span>
            <Show when={p.usage?.codex_plan_type}>
              <span class="p-dot">·</span><span>{p.usage!.codex_plan_type} plan</span>
            </Show>
            <Show when={a.account_email && !a.name.includes(a.account_email)}>
              <span class="p-dot">·</span>
              {a.account_email}
            </Show>
          </div>
        </div>
        <Show when={!open() && p.usage && !p.usage!.error}>
          <UsageSummary usage={p.usage!} />
        </Show>
        <Show when={expandable()}><span class={`chev p-acc-chev ${open() ? "open" : ""}`}>›</span></Show>
      </Dynamic>
      <Show when={open() && expandable()}>
        <div class="p-acc-body">
          <Show when={hasProviderUsageDetails(p.usage)}>
            <UsageDetail usage={p.usage!} headerEmail={a.account_email ?? (p.usage?.account_email && a.name.includes(p.usage.account_email) ? p.usage.account_email : undefined)} planInHeader />
          </Show>
          <Show when={!p.usage?.error && (a.status.reason || a.status.message)}><p class="s-row-desc c-red">{a.status.reason || a.status.message}</p></Show>
          <Show when={canReconnect()}>
            <div class="p-acc-actions">
              <button class="s-btn" onClick={p.onReconnect}>
                {stClass() === "connected" ? "Re-auth" : "Reconnect"}
              </button>
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function AccountRow(p: { a: Account; onSignIn: () => void; onToggle: () => void; onRemove: () => void }) {
  const a = p.a;
  const st = a.status === "connected" ? "Connected" : a.status === "needs_reauth" ? "Reconnect" : "Not configured";
  return (
    <div class="s-row p-acc">
      <ProviderLogo type={a.type} name={a.name} />
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
