import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import {
  connectionVersion,
  getApiUrl,
  isConnected,
  listProviders,
  type AIProvider,
} from "./api";
import * as R from "./routingApi";
import { ErrorNotice } from "./ErrorNotice";
import { Dialog } from "./Dialog";

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
  async function refresh(full = false) {
    if (!isConnected()) return;
    if (fetching) {
      queuedFullRefresh ||= full;
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
      read("Provider Health", R.listHealth, setHealth),
      read("Recent Fallback Events", R.listEvents, setEvents),
      ...(full
        ? [
            read("Fallback Chains", R.listChains, setChains),
            read("Model catalog", R.routingCatalog, setCatalog),
            read("Accounts", listProviders, setAccounts),
          ]
        : []),
    ]);
    if (epoch === generation) {
      setUpdated(new Date().toISOString());
      setLoading(false);
      fetching = false;
      if (queuedFullRefresh) {
        queuedFullRefresh = false;
        void refresh(true);
      }
    }
  }
  createEffect(() => {
    connectionVersion();
    const connected = isConnected();
    generation++;
    fetching = false;
    queuedFullRefresh = false;
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
    setDirty(false);
    setResult(undefined);
    setChainFilter("");
    setProviderFilter("");
    setReasonFilter("");
    setBusy(false);
    setActionError("");
    if (connected) void refresh(true);
  });
  const poll = setInterval(() => {
    if (document.visibilityState !== "hidden") void refresh();
  }, 10000);
  const onVisibility = () => {
    if (document.visibilityState !== "hidden") void refresh();
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
  function edit(chain?: R.ModelChain) {
    if (busy() || !confirmLeaveRouting(() => edit(chain))) return;
    setEditing(chain?.id ?? "");
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
        await refresh(true);
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
      },
    );
  }
  async function inspect(id: string, test: boolean) {
    const epoch = generation;
    await action(async () => {
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
  const Editor = () => (
    <form
      class="s-card routing-editor"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <h3>{editing() === "" ? "New chain" : `Edit ${editing()}`}</h3>
      <fieldset disabled={busy()}>
        <label>
          Name
          <input
            class="s-input"
            value={draft().name}
            onInput={(e) => change({ name: e.currentTarget.value })}
          />
        </label>
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
        <p class="routing-muted">
          Entries are tried in order. Drag an entry or use the arrow buttons.
        </p>
        <datalist id="routing-provider-options">
          <For each={catalog().providers}>
            {(provider) => <option value={provider.id}>{provider.name}</option>}
          </For>
        </datalist>
        <Index each={draft().entries}>
          {(e, index) => (
            <div
              class="routing-entry"
              draggable
              onDragStart={() => {
                dragged = index;
              }}
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
              <span class="routing-muted">{index + 1}</span>
              <label>
                Provider
                <input
                  class="s-input"
                  aria-label={`Provider ${index + 1}`}
                  list="routing-provider-options"
                  value={e().provider_id}
                  onInput={(event) =>
                    entry(index, { provider_id: event.currentTarget.value })
                  }
                />
              </label>
              <label>
                Model
                <input
                  class="s-input"
                  aria-label={`Model ${index + 1}`}
                  list={`routing-models-${index}`}
                  value={e().model_id}
                  onInput={(event) =>
                    entry(index, { model_id: event.currentTarget.value })
                  }
                />
                <datalist id={`routing-models-${index}`}>
                  <For
                    each={
                      catalog().providers.find((p) => p.id === e().provider_id)
                        ?.models ?? []
                    }
                  >
                    {(model) => <option value={model.id}>{model.name}</option>}
                  </For>
                </datalist>
              </label>
              <div class="routing-actions">
                <button
                  type="button"
                  class="s-btn"
                  aria-label={`Move entry ${index + 1} up`}
                  disabled={index === 0}
                  onClick={() => move(index, index - 1)}
                >
                  ↑
                </button>
                <button
                  type="button"
                  class="s-btn"
                  aria-label={`Move entry ${index + 1} down`}
                  disabled={index === draft().entries.length - 1}
                  onClick={() => move(index, index + 1)}
                >
                  ↓
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
                  Remove
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
          Add entry
        </button>
        <details class="routing-advanced">
          <summary>Advanced</summary>
          <label class="routing-check">
            <input
              type="checkbox"
              checked={draft().strip_thinking}
              onChange={(e) =>
                change({ strip_thinking: e.currentTarget.checked })
              }
            />
            Strip thinking blocks from responses
          </label>
        </details>
        <div class="routing-actions">
          <button class="s-btn primary" type="submit">
            Save
          </button>
          <button
            class="s-btn"
            type="button"
            onClick={() => {
              if (
                confirmLeaveRouting(() => {
                  setEditing(null);
                  setActionError("");
                })
              ) {
                setEditing(null);
                setDirty(false);
                setActionError("");
              }
            }}
          >
            Cancel
          </button>
          <Show when={dirty()}>
            <span class="routing-muted">Unsaved changes</span>
          </Show>
        </div>
      </fieldset>
    </form>
  );
  return (
    <div class="s-body">
      <div class="s-inner routing-page">
        <div class="routing-heading">
          <h2>Routing</h2>
          <button
            class="s-btn"
            disabled={loading() || !isConnected()}
            onClick={() => void refresh(true)}
          >
            {loading() ? "Refreshing…" : "Refresh"}
          </button>
        </div>
        <p class="s-lead">
          {serverUrl()}
          <br />
          Routes requests through this server’s proxy. Local agents using
          providers directly keep their own configuration.
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
          <p class="routing-muted" role="status">
            {updated()
              ? `Last refresh ${time(updated()!)}${Object.keys(errors()).length ? " · Some data could not be refreshed" : ""}`
              : "Loading routing…"}
          </p>
          <nav class="routing-jumps" aria-label="Routing sections">
            <a href="#routing-chains">Fallback Chains</a>
            <a href="#routing-health">Provider Health</a>
            <a href="#routing-events">Recent Fallback Events</a>
          </nav>
          <For each={Object.entries(errors())}>
            {([key, error]) => <ErrorNotice error={`${key}: ${error}`} />}
          </For>
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
                  <button
                    class="routing-chain-summary"
                    aria-expanded={editing() === chain.id}
                    onClick={() => edit(chain)}
                  >
                    <strong>{chain.name}</strong> <code>{chain.id}</code>
                    <Show when={chain.is_default}>
                      <span class="routing-badge">Default</span>
                    </Show>
                    <span class="routing-order">
                      {chain.entries
                        .map((e) => `${e.provider_id} / ${e.model_id}`)
                        .join(" → ")}
                    </span>
                  </button>
                  <div class="routing-actions">
                    <button
                      class="s-btn"
                      disabled={busy()}
                      onClick={() => void inspect(chain.id, false)}
                    >
                      Resolve
                    </button>
                    <button
                      class="s-btn"
                      disabled={busy() || dirty()}
                      title="Sends a small inference request using the saved chain; may consume provider quota"
                      onClick={() => void inspect(chain.id, true)}
                    >
                      Test request
                    </button>
                    <Show when={!chain.is_default}>
                      <button
                        class="s-btn"
                        disabled={busy() || dirty()}
                        onClick={() =>
                          void action(() => R.setDefaultChain(chain.id))
                        }
                      >
                        Set as default
                      </button>
                    </Show>
                    <Show when={!chain.id.startsWith("builtin/")}>
                      <button
                        class="s-btn"
                        disabled={busy() || dirty() || chain.is_default}
                        title={
                          chain.is_default
                            ? "Choose another default before deleting"
                            : "Delete chain"
                        }
                        onClick={() => setDeleteTarget(chain)}
                      >
                        Delete
                      </button>
                    </Show>
                  </div>
                  <Show when={result()?.id === chain.id}>
                    <div class="routing-result">
                      <Show when={result()?.resolved}>
                        {(resolved) => (
                          <>
                            <p>
                              Currently eligible accounts · no inference request
                              sent
                            </p>
                            <Show
                              when={resolved().length}
                              fallback={<p>No eligible accounts.</p>}
                            >
                              <For each={resolved()}>
                                {(e) => (
                                  <p>
                                    {e.provider_id} / {e.model_id} ·{" "}
                                    {accountName(e.account_id)} · {e.auth_kind}
                                    {e.has_credentials
                                      ? ""
                                      : " · Missing credentials"}
                                  </p>
                                )}
                              </For>
                            </Show>
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
          <section class="s-sec" id="routing-events">
            <h3>Recent Fallback Events</h3>
            <p class="routing-muted">
              Up to 200 recent events retained by the backend. This is not a
              permanent log.
            </p>
            <div class="routing-filters">
              <label>
                Chain
                <select
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
                </select>
              </label>
              <label>
                Provider
                <select
                  value={providerFilter()}
                  onChange={(e) => setProviderFilter(e.currentTarget.value)}
                >
                  <option value="">All providers</option>
                  <For each={eventProviders()}>
                    {(id) => <option>{id}</option>}
                  </For>
                </select>
              </label>
              <label>
                Reason
                <select
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
                </select>
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
        <Show when={leaveRequest()}>
          {(callback) => (
            <Dialog
              title="Discard unsaved changes?"
              onClose={() => setLeaveRequest(null)}
              footer={
                <>
                  <button class="s-btn" onClick={() => setLeaveRequest(null)}>
                    Keep editing
                  </button>
                  <button
                    class="s-btn"
                    onClick={() => {
                      const next = callback();
                      setDirty(false);
                      setLeaveRequest(null);
                      next();
                    }}
                  >
                    Discard changes
                  </button>
                </>
              }
            >
              <p>Your routing changes have not been saved.</p>
            </Dialog>
          )}
        </Show>
        <Show when={deleteTarget()}>
          {(chain) => (
            <Dialog
              title="Delete chain?"
              onClose={() => {
                if (!busy()) setDeleteTarget(null);
              }}
              footer={
                <>
                  <button
                    class="s-btn"
                    disabled={busy()}
                    onClick={() => setDeleteTarget(null)}
                  >
                    Cancel deletion
                  </button>
                  <button
                    class="s-btn"
                    disabled={busy()}
                    onClick={() => {
                      const id = chain().id;
                      void action(
                        () => R.deleteChain(id),
                        () => {
                          setDeleteTarget(null);
                          if (editing() === id) setEditing(null);
                        },
                      );
                    }}
                  >
                    Delete chain
                  </button>
                </>
              }
            >
              <p>Delete “{chain().name}”? This cannot be undone.</p>
              <Show when={actionError()}>
                <ErrorNotice error={actionError()} />
              </Show>
            </Dialog>
          )}
        </Show>
      </div>
    </div>
  );
}
