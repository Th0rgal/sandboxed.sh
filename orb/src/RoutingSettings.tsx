import { Select } from "./Select";
import { RoutingPicker } from "./RoutingPicker";
import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import {
  connectionVersion,
  getApiUrl,
  isConnected,
  listProviders,
  type AIProvider,
} from "./api";
import * as R from "./routingApi";
import * as Ic from "./icons";
import { ErrorNotice } from "./ErrorNotice";
import { ConfirmDialog } from "./Dialog";

const [dirty, setDirty] = createSignal(false);
const [leaveRequest, setLeaveRequest] = createSignal<(() => void) | null>(null);
export function confirmLeaveRouting(onDiscard: () => void = () => {}) {
  if (!dirty()) return true;
  setLeaveRequest(() => onDiscard);
  return false;
}
const emptyDraft = (): R.ChainDraft => ({
  id: "",
  name: "",
  entries: [{ provider_id: "", model_id: "" }],
  strip_thinking: false,
});
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const time = (value: string | null) =>
  value ? new Date(value).toLocaleString() : "—";

export function RoutingSettings(p: { onOpenClient: () => void }) {
  const serverUrl = createMemo(() => {
    connectionVersion();
    return getApiUrl();
  });
  const [tab, setTab] = createSignal<"chains" | "health" | "events">("chains");
  const [catalogLoading, setCatalogLoading] = createSignal(false);
  const [copiedId, setCopiedId] = createSignal("");
  let copyFeedbackTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copyFeedbackTimer));
  const [chains, setChains] = createSignal<R.ModelChain[]>([]);
  const [health, setHealth] = createSignal<R.AccountHealthSnapshot[]>([]);
  const [events, setEvents] = createSignal<R.FallbackEvent[]>([]);
  const [catalog, setCatalog] = createSignal<R.RoutingCatalog>({
    providers: [],
  });
  const [accounts, setAccounts] = createSignal<AIProvider[]>([]);
  const [errors, setErrors] = createSignal<Record<string, string>>({});
  const [updated, setUpdated] = createSignal<string>();
  const [loading, setLoading] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [expanded, setExpanded] = createSignal<string | null>(null);
  const [renaming, setRenaming] = createSignal(false);
  const [editing, setEditing] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal<R.ChainDraft>(emptyDraft());
  const [deleteTarget, setDeleteTarget] = createSignal<R.ModelChain | null>(
    null,
  );
  const [actionError, setActionError] = createSignal("");
  const [result, setResult] = createSignal<{
    id: string;
    resolved?: R.ResolvedEntry[];
    test?: R.ChainTestResult;
  }>();
  const [chainFilter, setChainFilter] = createSignal("");
  const [providerFilter, setProviderFilter] = createSignal("");
  const [expandedHealth, setExpandedHealth] = createSignal<Set<string>>(
    new Set(),
  );
  const [reasonFilter, setReasonFilter] = createSignal("");
  let generation = 0;
  let fetching = false;
  let queuedFullRefresh = false;
  let queuedForceRefresh = false;
  let dragged = -1;
  const accountName = (id: string) => {
    const account = accounts().find((a) => a.id === id);
    return (
      account?.label ||
      account?.account_email ||
      account?.name ||
      id.slice(0, 8)
    );
  };
  const clearError = (key: string, value?: string) =>
    setErrors((current) => {
      const next = { ...current };
      if (value) next[key] = value;
      else delete next[key];
      return next;
    });
  async function refresh(full = false, force = false) {
    if (!isConnected()) return;
    if (fetching) {
      queuedFullRefresh ||= full;
      queuedForceRefresh ||= force;
      return;
    }
    fetching = true;
    const epoch = generation;
    if (full) setLoading(true);
    const read = async <T,>(
      key: string,
      fn: () => Promise<T>,
      apply: (value: T) => unknown,
    ) => {
      try {
        const value = await fn();
        if (epoch === generation) {
          apply(value);
          clearError(key);
        }
      } catch (e) {
        if (epoch === generation) clearError(key, message(e));
      }
    };
    await Promise.all([
      ...(full ? [read("Fallback Chains", () => R.listChains(force), setChains)] : []),
      ...(tab() === "health" ? [
        read("Provider Health", R.listHealth, setHealth),
        ...(full ? [read("Accounts", listProviders, setAccounts)] : []),

      ] : []),
      ...(tab() === "events" ? [read("Recent Fallback Events", R.listEvents, setEvents)] : []),
    ]);
    if (epoch === generation) {
      setUpdated(new Date().toISOString());
      setLoading(false);
      fetching = false;
      if (queuedFullRefresh) {
        queuedFullRefresh = false;
        const force = queuedForceRefresh;
        queuedForceRefresh = false;
        void refresh(true, force);
      }
    }
  }
  createEffect(() => {
    connectionVersion();
    const connected = isConnected();
    generation++;
    fetching = false;
    queuedFullRefresh = false;
    queuedForceRefresh = false;
    setChains([]);
    setHealth([]);
    setEvents([]);
    setAccounts([]);
    setCatalog({ providers: [] });
    setLeaveRequest(null);
    setDeleteTarget(null);
    setErrors({});
    setExpandedHealth(new Set<string>());
    setUpdated(undefined);
    setEditing(null);
    setExpanded(null);
    setDirty(false);
    setResult(undefined);
    setChainFilter("");
    setProviderFilter("");
    setReasonFilter("");
    setBusy(false);
    setActionError("");
    setCatalogLoading(false);
    if (connected) untrack(() => void refresh(true));
  });
  const poll = setInterval(() => {
    if (document.visibilityState !== "hidden" && tab() !== "chains") void refresh();
  }, 10000);
  const onVisibility = () => {
    if (document.visibilityState !== "hidden" && tab() !== "chains") void refresh();
  };
  const beforeUnload = (e: BeforeUnloadEvent) => {
    if (dirty()) {
      e.preventDefault();
      e.returnValue = "";
    }
  };
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("beforeunload", beforeUnload);
  onCleanup(() => {
    generation++;
    clearInterval(poll);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("beforeunload", beforeUnload);
    setDirty(false);
    setLeaveRequest(null);
  });
  function selectTab(next: "chains" | "health" | "events") {
    if (!confirmLeaveRouting(() => selectTab(next))) return;
    setEditing(null);
    setTab(next);
    void refresh(true);
  }
  const [discovery, setDiscovery] = createSignal<R.ModelDiscovery>();
  async function loadCatalog(force = false) {
    const epoch = generation;
    setCatalogLoading(true);
    try {
      if (force) await R.refreshModelDiscovery();
      const [value, evidence] = await Promise.all([R.routingCatalog(force), R.modelDiscovery().catch(() => undefined)]);
      if (epoch === generation) setDiscovery(evidence);
      if (epoch === generation) { setCatalog(value); clearError("Model catalog"); }
    } catch (error) {
      if (epoch === generation) clearError("Model catalog", message(error));
    } finally { if (epoch === generation) setCatalogLoading(false); }
  }
  function edit(chain?: R.ModelChain) {
    if (busy() || !confirmLeaveRouting(() => edit(chain))) return;
    void loadCatalog();
    setExpanded(chain?.id ?? null);
    setEditing(chain?.id ?? "");
    setRenaming(false);
    setDraft(
      chain
        ? {
            id: chain.id,
            name: chain.name,
            entries: chain.entries.map((e) => ({ ...e })),
            strip_thinking: chain.strip_thinking,
          }
        : emptyDraft(),
    );
    setDirty(false);
    setActionError("");
    setResult(undefined);
  }
  function change(patch: Partial<R.ChainDraft>) {
    setDraft((current) => ({ ...current, ...patch }));
    setDirty(true);
  }
  function entry(index: number, patch: Partial<R.ChainEntry>) {
    change({
      entries: draft().entries.map((e, i) =>
        i === index ? { ...e, ...patch } : e,
      ),
    });
  }
  function move(from: number, to: number) {
    if (from < 0 || to < 0 || to >= draft().entries.length) return;
    const entries = [...draft().entries];
    entries.splice(to, 0, entries.splice(from, 1)[0]);
    change({ entries });
  }
  async function action(fn: () => Promise<unknown>, done?: () => void) {
    if (busy()) return;
    const epoch = generation;
    setBusy(true);
    setActionError("");
    try {
      await fn();
      if (epoch === generation) {
        done?.();
        await refresh(true, true);
      }
    } catch (e) {
      if (epoch === generation) setActionError(message(e));
    } finally {
      if (epoch === generation) setBusy(false);
    }
  }
  function save() {
    const value = {
      ...draft(),
      id: draft().id.trim(),
      name: draft().name.trim(),
      entries: draft().entries.map((e) => ({
        provider_id: e.provider_id.trim(),
        model_id: e.model_id.trim(),
      })),
    };
    if (
      !value.id ||
      !value.name ||
      !value.entries.length ||
      value.entries.some((e) => !e.provider_id || !e.model_id)
    ) {
      setActionError(
        "A name, ID and at least one complete provider/model entry are required.",
      );
      return;
    }
    if (editing() === "" && value.id.startsWith("builtin/")) {
      setActionError("The builtin/ prefix is reserved.");
      return;
    }
    void action(
      () => R.saveChain(value, editing() !== ""),
      () => {
        setDirty(false);
        setEditing(null);
        setExpanded(null);
      },
    );
  }
  async function inspect(id: string, test: boolean) {
    const epoch = generation;
    await action(async () => {
      if (!test) {
        try { const rows = await listProviders(); if (epoch === generation) setAccounts(rows); } catch { /* Resolution still works with account IDs. */ }
      }
      const value = test
        ? { id, test: await R.testChain(id) }
        : { id, resolved: await R.resolveChain(id) };
      if (epoch === generation) setResult(value);
    });
  }
  const eventProviders = createMemo(() =>
    [
      ...new Set(
        events().flatMap((e) =>
          [e.from_provider, e.to_provider].filter((s): s is string => !!s),
        ),
      ),
    ].sort(),
  );
  const filteredEvents = createMemo(() =>
    events()
      .filter(
        (e) =>
          (!chainFilter() || e.chain_id === chainFilter()) &&
          (!providerFilter() ||
            e.from_provider === providerFilter() ||
            e.to_provider === providerFilter()) &&
          (!reasonFilter() || e.reason === reasonFilter()),
      )
      .sort((a, b) => b.timestamp.localeCompare(a.timestamp)),
  );
  const ChainTools = (props: { chain: R.ModelChain }) => {
    return <div class="routing-actions routing-diagnostics">
      <button type="button" class="s-btn" disabled={busy() || dirty()} title="Test the saved chain with a small inference request" onClick={() => void inspect(props.chain.id, true)}>Test chain</button>
      <button type="button" class="s-btn" disabled={busy()} onClick={() => {
        setRenaming(true);
        queueMicrotask(() => document.getElementById("routing-chain-name")?.focus());
      }}>Rename</button>
      <button type="button" class="s-btn" disabled={busy() || dirty()} onClick={() => void inspect(props.chain.id, false)}>Resolve</button>
      <Show when={!props.chain.is_default}>
        <button type="button" class="s-btn" disabled={busy() || dirty()} onClick={() => void action(() => R.setDefaultChain(props.chain.id))}>Set as default</button>
        <Show when={!props.chain.id.startsWith("builtin/")}>
          <button type="button" class="s-btn" disabled={busy() || dirty()} onClick={() => setDeleteTarget(props.chain)}>Delete</button>
        </Show>
      </Show>
    </div>;
  };
  const ChainResult = (props: { chain: R.ModelChain }) => { const chain = props.chain; return <>
                  <Show when={result()?.id === chain.id}>
                    <div class="routing-result">
                      <Show when={result()?.resolved}>
                        {(resolved) => (
                          <>
                            <details class="routing-resolved">
                            <summary>{resolved().length} eligible accounts</summary>
                            <p class="routing-muted">No inference request sent.</p>
                            <Show
                              when={resolved().length}
                              fallback={<p>No eligible accounts.</p>}
                            >
                              <For each={resolved()}>
                                {(e) => (
                                  <p>
                                    {e.provider_id} / {e.model_id} ·{" "}
                                    {accountName(e.account_id)} · {e.auth_kind === "api_key" ? "API key" : e.auth_kind === "oauth" ? "OAuth" : "No credentials"}
                                    {e.has_credentials
                                      ? ""
                                      : " · Missing credentials"}
                                  </p>
                                )}
                              </For>
                            </Show>
                            <For each={chain.entries.filter(entry => !resolved().some(r => r.provider_id === entry.provider_id && r.model_id === entry.model_id))}>
                              {entry => <p class="routing-muted">{entry.provider_id} / {entry.model_id} — No eligible account. The server did not provide an exclusion reason.</p>}
                            </For>
                            </details>
                          </>
                        )}
                      </Show>
                      <Show when={result()?.test}>
                        {(test) => (
                          <>
                            <p role="status">
                              {test().ok
                                ? "Request succeeded"
                                : "Request failed"}{" "}
                              · HTTP {test().status}
                            </p>
                            <Show when={test().ok}>
                              <p class="routing-test-model">
                                Model used: <strong>{typeof test().response.model === "string" && test().response.model!.trim()
                                  ? test().response.model
                                  : "Not reported by provider"}</strong>
                              </p>
                            </Show>
                            <p>
                              {test().response.error?.message ||
                                test().response.choices?.[0]?.message.content ||
                                "No response text."}
                            </p>
                            <p class="routing-muted">
                              This tests the saved chain, not every fallback
                              entry.
                            </p>
                          </>
                        )}
                      </Show>
                    </div>
                  </Show>
  </>; };
  const Editor = () => (
    <form
      class="routing-editor"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <Show when={editing() === ""}><h3>New chain</h3></Show>
      <fieldset disabled={busy()}>
        <Show when={editing() === "" || renaming()}>
        <label>
          Name
          <input
            class="s-input"
            id="routing-chain-name"
            value={draft().name}
            onInput={(e) => change({ name: e.currentTarget.value })}
          />
        </label>
        </Show>
        <Show when={editing() === ""}>
        <label>
          Chain ID
          <input
            class="s-input"
            disabled={editing() !== ""}
            value={draft().id}
            onInput={(e) => change({ id: e.currentTarget.value })}
            placeholder="my-chain"
          />
        </label>
        </Show>
        <p class="routing-muted">Tried from top to bottom. Drag to reorder.</p>
        <div class="routing-actions routing-catalog-status">
          <span class="routing-muted">Suggestions prioritize connected accounts. Listing does not verify inference.</span>
          <button type="button" class="s-btn" disabled={catalogLoading()} onClick={() => void loadCatalog(true)}>Refresh models</button>
        </div>
        <Show when={catalogLoading()}><p class="routing-muted" role="status">Loading model suggestions…</p></Show>
        <div class="routing-entry-head" aria-hidden="true"><span /><span>Provider</span><span>Model</span><span /></div>
        <Index each={draft().entries}>
          {(e, index) => (
            <div
              class="routing-entry"
              onDragOver={(event) => event.preventDefault()}
              onDrop={(event) => {
                event.preventDefault();
                move(dragged, index);
                dragged = -1;
              }}
              onDragEnd={() => {
                dragged = -1;
              }}
            >
              <span class="routing-grip" draggable title="Drag to reorder" onDragStart={e => { dragged = index; e.dataTransfer?.setData("text/plain", String(index)); if (e.dataTransfer) e.dataTransfer.effectAllowed = "move"; }}>⠿</span>
              <RoutingPicker label={`Provider ${index + 1}`} value={e().provider_id} options={catalog().providers}
                onInput={value => entry(index, { provider_id: value })} />
              <RoutingPicker label={`Model ${index + 1}`} value={e().model_id}
                options={(catalog().providers.find(p => p.id === e().provider_id)?.models ?? []).map(m => ({ ...m, detail: R.modelEvidence(discovery(), e().provider_id, m.id) }))}
                onInput={value => entry(index, { model_id: value })} />
              <div class="routing-actions">
                <button
                  type="button"
                  class="s-btn"
                  aria-label={`Move entry ${index + 1} up`}
                  disabled={index === 0}
                  onClick={() => move(index, index - 1)}
                >
                  <span class="routing-up"><Ic.ArrowUpIcon size={13} /></span>
                </button>
                <button
                  type="button"
                  class="s-btn"
                  aria-label={`Move entry ${index + 1} down`}
                  disabled={index === draft().entries.length - 1}
                  onClick={() => move(index, index + 1)}
                >
                  <span class="routing-down"><Ic.ArrowUpIcon size={13} /></span>
                </button>
                <button
                  type="button"
                  class="s-btn"
                  aria-label={`Remove entry ${index + 1}`}
                  onClick={() =>
                    change({
                      entries: draft().entries.filter((_, i) => i !== index),
                    })
                  }
                >
                  <Ic.CloseIcon size={13} />
                </button>
              </div>
              <Show
                when={
                  e().provider_id &&
                  catalog().configured_ids &&
                  !catalog().configured_ids!.includes(e().provider_id)
                }
              >
                <span class="routing-warning">Provider not connected</span>
              </Show>
            </div>
          )}
        </Index>
        <button
          type="button"
          class="s-btn"
          onClick={() =>
            change({
              entries: [...draft().entries, { provider_id: "", model_id: "" }],
            })
          }
        >
          <Ic.PlusIcon size={13} /> Add fallback
        </button>
        <div class="routing-option-row">
          <div><div id="strip-thinking-label">Strip thinking blocks</div><p id="strip-thinking-help">Remove reasoning blocks from model responses.</p></div>
          <button type="button" class={`toggle ${draft().strip_thinking ? "on" : ""}`} role="switch" aria-labelledby="strip-thinking-label" aria-describedby="strip-thinking-help"
            aria-checked={draft().strip_thinking} onClick={() => change({ strip_thinking: !draft().strip_thinking })} />
        </div>
        <Show when={chains().find(c => c.id === editing())}>{chain => <ChainResult chain={chain()} />}</Show>
        <div class="routing-actions routing-savebar">
          <Show when={chains().find(c => c.id === editing())}>{chain => <ChainTools chain={chain()} />}</Show>
          <Show when={dirty() || editing() === ""}>
          <div class="routing-actions routing-save-actions">
          <Show when={dirty()}><span class="routing-save-status">Unsaved changes</span></Show>
          <button class="s-btn primary" type="submit" disabled={!dirty()}>
            Save
          </button>
          <button
            class="s-btn"
            type="button"
            onClick={() => {
              if (
                confirmLeaveRouting(() => {
                  setEditing(null);
                  setExpanded(null);
                  setActionError("");
                })
              ) {
                setEditing(null);
                setExpanded(null);
                setDirty(false);
                setActionError("");
              }
            }}
          >
            Cancel
          </button>
          </div>
          </Show>
        </div>
      </fieldset>
    </form>
  );
  return (
    <div class="s-body settings-body">
      <div class="s-inner routing-page">
        <div class="routing-heading">
          <h2>Routing</h2>
          <button
            class="s-btn"
            disabled={loading() || !isConnected()}
            onClick={() => void refresh(true, true)}
          >
            {loading() ? "Refreshing…" : "Refresh"}
          </button>
        </div>
        <p class="s-lead">
          Choose the order models are tried when a provider is unavailable.
          <span class="routing-server" title="Applies to this server’s proxy. Direct local providers keep their own configuration.">{serverUrl()}</span>
        </p>
        <Show
          when={isConnected()}
          fallback={
            <>
              <p class="s-lead">Connect to a backend to configure routing.</p>
              <button class="s-btn" onClick={p.onOpenClient}>
                Open Client settings
              </button>
            </>
          }
        >
          <nav class="routing-tabs" aria-label="Routing sections">
            <button aria-current={tab() === "chains" ? "page" : undefined} onClick={() => selectTab("chains")}>Fallback Chains</button>
            <button aria-current={tab() === "health" ? "page" : undefined} onClick={() => selectTab("health")}>Provider Health</button>
            <button aria-current={tab() === "events" ? "page" : undefined} onClick={() => selectTab("events")}>Recent Fallback Events</button>
          </nav>
          <For each={Object.entries(errors())}>
            {([key, error]) => <ErrorNotice error={`${key}: ${error}`} />}
          </For>
          <Show when={tab() === "chains"}>
          <section class="s-sec" id="routing-chains">
            <div class="routing-heading">
              <h3>Fallback Chains</h3>
              <button class="s-btn" disabled={busy()} onClick={() => edit()}>
                New chain
              </button>
            </div>
            <Show when={!loading() && !chains().length}>
              <p class="routing-muted">
                No chains configured. The backend creates builtin/smart when
                first needed.
              </p>
            </Show>
            <Show when={editing() === ""}>
              <Editor />
            </Show>
            <For each={chains()}>
              {(chain) => (
                <article class="s-card routing-chain">
                  <div class="routing-chain-heading">
                  <button
                    class="routing-chain-summary"
                    aria-expanded={expanded() === chain.id}
                    onClick={() => {
                      const next = expanded() === chain.id ? null : chain.id;
                      const apply = () => { if (next) edit(chain); else { setEditing(null); setExpanded(null); } };
                      if (confirmLeaveRouting(apply)) apply();
                    }}
                  >
                    <strong>{chain.is_default ? chain.name.replace(/\s*\(Default\)\s*$/i, "") : chain.name}</strong>
                    <code>{chain.id}</code>
                    <Show when={chain.is_default}>
                      <span class="routing-badge" title="Default chain metadata; conversations and proxy requests keep their explicitly selected model">Default</span>
                    </Show>
                    <span class="routing-edit-icon"><Ic.ChevronDown size={14} /></span>
                    <Show when={expanded() !== chain.id}><span class="routing-order">
                      {chain.entries
                        .map((e) => e.model_id)
                        .join(" → ")}
                    </span></Show>
                  </button>
                  <button type="button" class="routing-copy" aria-label={`Copy model ID ${chain.id}`} title={copiedId() === chain.id ? "Copied!" : "Copy model ID"}
                    onClick={async () => { try { await navigator.clipboard.writeText(chain.id); setCopiedId(chain.id); clearTimeout(copyFeedbackTimer); copyFeedbackTimer = setTimeout(() => setCopiedId(""), 1800); } catch { setActionError("Could not copy model ID."); } }}>
                    <Show when={copiedId() === chain.id} fallback={<Ic.CopyIcon size={13} />}><span aria-live="polite">✓</span></Show>
                  </button>
                  </div>
                  <Show when={editing() === chain.id}>
                    <Editor />
                  </Show>
                </article>
              )}
            </For>
            <Show when={actionError()}>
              <ErrorNotice error={actionError()} />
            </Show>
          </section>
          </Show>
          <Show when={tab() === "health"}>
          <section class="s-sec" id="routing-health">
            <h3>Provider Health</h3>
            <p class="routing-muted">
              Activity observed by this proxy. Token counts are not subscription
              quota balances.
            </p>
            <Show
              when={health().length}
              fallback={
                <p class="routing-muted">
                  No health data yet. Tracking begins when the proxy handles
                  requests.
                </p>
              }
            >
              <For each={health()}>
                {(h) => (
                  <details
                    class="s-card routing-health"
                    open={expandedHealth().has(h.account_id)}
                    onToggle={(e) => {
                      const open = e.currentTarget.open;
                      setExpandedHealth((current) => {
                        const next = new Set(current);
                        if (open) next.add(h.account_id);
                        else next.delete(h.account_id);
                        return next;
                      });
                    }}
                  >
                    <summary>
                      <strong>{accountName(h.account_id)}</strong>
                      <span>{h.provider_id || h.account_id.slice(0, 8)}</span>
                      <span
                        class={`routing-status ${h.cooldown_remaining_secs ? "bad" : h.is_degraded ? "warn" : h.total_requests && h.is_healthy ? "good" : ""}`}
                      >
                        {h.cooldown_remaining_secs
                          ? `Cooldown · ${Math.ceil(h.cooldown_remaining_secs)}s`
                          : h.is_degraded
                            ? "Degraded"
                            : !h.total_requests
                              ? "No data"
                              : h.is_healthy
                                ? "Healthy"
                                : "Unavailable"}
                      </span>
                      <span class="routing-order">
                        {h.total_requests} requests ·{" "}
                        {h.total_requests
                          ? `${Math.round((h.total_successes / h.total_requests) * 100)}% success`
                          : "—"}{" "}
                        · {h.total_errors} errors · {h.total_rate_limits} rate
                        limits ·{" "}
                        {h.avg_latency_ms == null
                          ? "—"
                          : `${Math.round(h.avg_latency_ms)} ms`}
                      </span>
                    </summary>
                    <div class="routing-result">
                      <p>Account: {h.account_id}</p>
                      <p>
                        Last failure: {h.last_failure_reason || "None"} ·{" "}
                        {time(h.last_failure_at)}
                      </p>
                      <p>
                        {h.consecutive_failures} consecutive failures ·{" "}
                        {h.total_input_tokens.toLocaleString()} input tokens ·{" "}
                        {h.total_output_tokens.toLocaleString()} output tokens
                      </p>
                      <Show when={h.rate_limit_snapshot}>
                        {(s) => (
                          <>
                            <p>
                              Provider limits · observed {time(s().updated_at)}
                            </p>
                            <p>
                              Requests remaining:{" "}
                              {s().requests_remaining ?? "Unknown"} /{" "}
                              {s().requests_limit ?? "Unknown"} · reset{" "}
                              {time(s().requests_reset)}
                            </p>
                            <p>
                              Tokens remaining:{" "}
                              {s().tokens_remaining ?? "Unknown"} /{" "}
                              {s().tokens_limit ?? "Unknown"} · reset{" "}
                              {time(s().tokens_reset)}
                            </p>
                          </>
                        )}
                      </Show>
                      <Show
                        when={
                          h.cooldown_remaining_secs != null &&
                          h.cooldown_remaining_secs! > 0
                        }
                      >
                        <button
                          class="s-btn"
                          disabled={busy()}
                          onClick={() =>
                            void action(() => R.clearCooldown(h.account_id))
                          }
                        >
                          Clear cooldown
                        </button>
                      </Show>
                    </div>
                  </details>
                )}
              </For>
            </Show>
          </section>
          </Show>
          <Show when={tab() === "events"}>
          <section class="s-sec" id="routing-events">
            <h3>Recent Fallback Events</h3>
            <p class="routing-muted">
              Up to 200 recent events retained by the backend. This is not a
              permanent log.
            </p>
            <div class="routing-filters">
              <label>
                Chain
                <Select
                  aria-label="Chain"
                  value={chainFilter()}
                  onChange={(e) => setChainFilter(e.currentTarget.value)}
                >
                  <option value="">All chains</option>
                  <For
                    each={[
                      ...new Set([
                        ...chains().map((c) => c.id),
                        ...events().map((e) => e.chain_id),
                      ]),
                    ]}
                  >
                    {(id) => <option value={id}>{id}</option>}
                  </For>
                </Select>
              </label>
              <label>
                Provider
                <Select
                  aria-label="Provider"
                  value={providerFilter()}
                  onChange={(e) => setProviderFilter(e.currentTarget.value)}
                >
                  <option value="">All providers</option>
                  <For each={eventProviders()}>
                    {(id) => <option>{id}</option>}
                  </For>
                </Select>
              </label>
              <label>
                Reason
                <Select
                  aria-label="Reason"
                  value={reasonFilter()}
                  onChange={(e) => setReasonFilter(e.currentTarget.value)}
                >
                  <option value="">All reasons</option>
                  <For each={[...new Set(events().map((e) => e.reason))]}>
                    {(reason) => (
                      <option value={reason}>
                        {reason.replaceAll("_", " ")}
                      </option>
                    )}
                  </For>
                </Select>
              </label>
            </div>
            <Show
              when={filteredEvents().length}
              fallback={
                <p class="routing-muted">
                  {events().length
                    ? "No events match these filters."
                    : "No fallback events recorded."}
                </p>
              }
            >
              <For each={filteredEvents()}>
                {(event) => (
                  <div class="routing-event">
                    <div class="routing-heading">
                      <time>{time(event.timestamp)}</time>
                      <button
                        class="s-btn"
                        disabled={
                          !chains().some((c) => c.id === event.chain_id)
                        }
                        onClick={() => {
                          const chain = chains().find(
                            (c) => c.id === event.chain_id,
                          );
                          if (chain) {
                            edit(chain);
                            document
                              .getElementById("routing-chains")
                              ?.scrollIntoView({ behavior: "smooth" });
                          }
                        }}
                      >
                        {event.chain_id}
                      </button>
                    </div>
                    <p>
                      {event.from_provider} / {event.from_model} ·{" "}
                      {accountName(event.from_account_id)} →{" "}
                      {event.to_provider || "Chain exhausted"}
                    </p>
                    <p class="routing-muted">
                      {event.reason.replaceAll("_", " ")} · attempt{" "}
                      {event.attempt_number}/{event.chain_length}
                      {event.latency_ms != null
                        ? ` · ${event.latency_ms} ms`
                        : ""}
                      {event.cooldown_secs != null
                        ? ` · ${event.cooldown_secs}s cooldown`
                        : ""}
                    </p>
                  </div>
                )}
              </For>
            </Show>
          </section>
          </Show>
          <p class="routing-updated" role="status">{loading() ? "Refreshing…" : updated() ? `Updated ${time(updated()!)}` : "Loading routing…"}</p>
        </Show>
        <Show when={leaveRequest()}>
          {(callback) => (
            <ConfirmDialog title="Discard unsaved changes?" description="Your routing changes have not been saved."
              action="Discard changes" cancelLabel="Keep editing" destructive onClose={() => setLeaveRequest(null)}
              onConfirm={() => { const next = callback(); setDirty(false); setLeaveRequest(null); next(); }} />
          )}
        </Show>
        <Show when={deleteTarget()}>
          {(chain) => (
            <ConfirmDialog title="Delete chain?" description={`Delete “${chain().name}”? This cannot be undone.`}
              action="Delete chain" cancelLabel="Cancel deletion" destructive busy={busy()} error={actionError()}
              onClose={() => setDeleteTarget(null)} onConfirm={() => {
                const id = chain().id;
                void action(() => R.deleteChain(id), () => { setDeleteTarget(null); if (editing() === id) setEditing(null); });
              }} />
          )}
        </Show>
      </div>
    </div>
  );
}
