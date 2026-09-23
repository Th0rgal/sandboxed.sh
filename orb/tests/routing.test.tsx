import { render, screen, fireEvent, waitFor } from "@solidjs/testing-library";
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { RoutingSettings, confirmLeaveRouting } from "../src/RoutingSettings";
import { setConnection, clearConnection } from "../src/api";
import type { ModelChain } from "../src/routingApi";
let chains: ModelChain[];
let writes: { path: string; method: string; body: any }[];
let failSave = false;
const chain = (): ModelChain => ({
  id: "test/chain",
  name: "Test chain",
  entries: [
    { provider_id: "anthropic", model_id: "legacy-model" },
    { provider_id: "openai", model_id: "gpt-test" },
  ],
  is_default: false,
  strip_thinking: false,
  created_at: "",
  updated_at: "",
});
const health = (id: string) => ({
  account_id: id,
  provider_id: "anthropic",
  is_healthy: false,
  cooldown_remaining_secs: 30,
  consecutive_failures: 1,
  last_failure_reason: "rate_limit",
  last_failure_at: null,
  total_requests: 10,
  total_successes: 9,
  total_rate_limits: 1,
  total_errors: 0,
  avg_latency_ms: 120,
  total_input_tokens: 100,
  total_output_tokens: 20,
  is_degraded: false,
  rate_limit_snapshot: null,
});
beforeEach(() => {
  chains = [chain()];
  writes = [];
  failSave = false;
  setConnection("https://routing.test", "test");
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: string, init?: RequestInit) => {
      const path = new URL(input).pathname;
      const method = init?.method || "GET";
      const body = init?.body ? JSON.parse(init.body as string) : undefined;
      if (method !== "GET") writes.push({ path, method, body });
      const response = (value: unknown, status = 200) =>
        new Response(JSON.stringify(value), { status });
      if (path === "/api/model-routing/chains" && method === "POST") {
        if (failSave) return response("Save failed", 503);
        chains.push({ ...body, is_default: false });
        return response(chains.at(-1));
      }
      if (path.startsWith("/api/model-routing/chains/") && method === "PUT") {
        if (failSave) return response("Save failed", 503);
        const id = decodeURIComponent(path.split("/").at(-1)!);
        chains = chains.map((c) => (c.id === id ? { ...c, ...body } : c));
        return response(chains.find((c) => c.id === id));
      }
      if (method === "DELETE") {
        chains = [];
        return response({ deleted: true });
      }
      if (path.endsWith("/resolve")) return response([]);
      if (path.endsWith("/test"))
        return response({
          ok: true,
          status: 200,
          response: { choices: [{ message: { content: "pong" } }] },
        });
      if (path.endsWith("/clear")) return response({ cleared: true });
      if (path.endsWith("/chains")) return response(chains);
      if (path.endsWith("/health"))
        return response([health("account-one"), health("account-two")]);
      if (path.endsWith("/events"))
        return response([
          {
            timestamp: "2026-09-23T10:00:00Z",
            chain_id: "test/chain",
            from_provider: "anthropic",
            from_model: "legacy-model",
            from_account_id: "account-one",
            reason: "rate_limit",
            cooldown_secs: 30,
            to_provider: null,
            latency_ms: 120,
            attempt_number: 2,
            chain_length: 2,
          },
        ]);
      if (path === "/api/ai/providers")
        return response([
          { id: "account-one", name: "First account" },
          { id: "account-two", name: "Second account" },
        ]);
      if (path === "/api/providers")
        return response({
          providers: [
            {
              id: "anthropic",
              name: "Anthropic",
              models: [{ id: "new-model", name: "New model" }],
            },
          ],
          configured_ids: ["anthropic"],
        });
      throw Error(path);
    }),
  );
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function mount() {
  render(() => <RoutingSettings onOpenClient={() => {}} />);
  await screen.findByRole("button", { name: /Test chain test\/chain/ });
  await screen.findByRole("button", { name: "Refresh", exact: true });
}
describe("Routing settings", () => {
  it("keeps unknown IDs, input focus and edited order on save", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: /Test chain test\/chain/ }),
    );
    const input = screen.getByLabelText("Model 1") as HTMLInputElement;
    input.focus();
    fireEvent.input(input, { target: { value: "custom-model" } });
    expect(document.activeElement).toBe(input);
    expect(screen.getByLabelText("Model 1")).toBe(input);
    fireEvent.click(screen.getByRole("button", { name: "Move entry 1 down" }));
    expect(screen.getByLabelText("Model 2")).toHaveProperty(
      "value",
      "custom-model",
    );
    fireEvent.click(screen.getByRole("button", { name: "Save", exact: true }));
    await waitFor(() =>
      expect(writes[0]).toMatchObject({
        path: "/api/model-routing/chains/test%2Fchain",
        method: "PUT",
        body: {
          entries: [
            { provider_id: "openai", model_id: "gpt-test" },
            { provider_id: "anthropic", model_id: "custom-model" },
          ],
        },
      }),
    );
    await waitFor(() =>
      expect(screen.queryByText("Unsaved changes")).toBeNull(),
    );
  });
  it("preserves a draft across refresh and save failure and guards leaving", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: /Test chain test\/chain/ }),
    );
    fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Draft name" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh", exact: true }),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Refresh", exact: true }),
      ).toHaveProperty("disabled", false),
    );
    expect(screen.getByLabelText("Name")).toHaveProperty("value", "Draft name");
    failSave = true;
    fireEvent.click(screen.getByRole("button", { name: "Save", exact: true }));
    await screen.findByText(/503.*Save failed/);
    expect(screen.getByLabelText("Name")).toHaveProperty("value", "Draft name");
    expect(confirmLeaveRouting()).toBe(false);
  });
  it("creates a chain and changes the default", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: "New chain", exact: true }),
    );
    for (const [label, value] of [
      ["Name", "New fallback"],
      ["Chain ID", "new"],
      ["Provider 1", "anthropic"],
      ["Model 1", "new-model"],
    ])
      fireEvent.input(screen.getByLabelText(label), { target: { value } });
    fireEvent.click(screen.getByRole("button", { name: "Save", exact: true }));
    await screen.findByRole("button", { name: /New fallback new/ });
    await waitFor(() =>
      expect(
        screen.getAllByRole("button", { name: "Set as default" })[0],
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(
      screen.getAllByRole("button", { name: "Set as default" })[0],
    );
    await waitFor(() =>
      expect(writes.some((w) => w.body?.is_default)).toBe(true),
    );
  });
  it("shows separate accounts, exhaustion, resolution and explicit inference", async () => {
    await mount();
    expect(screen.getByText("First account")).toBeTruthy();
    expect(screen.getByText("Second account")).toBeTruthy();
    expect(screen.getByText(/→ Chain exhausted/)).toBeTruthy();
    expect(writes).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Resolve" }));
    await screen.findByText("No eligible accounts.");
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Test request" }),
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(screen.getByRole("button", { name: "Test request" }));
    await screen.findByText("pong");
  });
  it("requires an integrated confirmation before deletion", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: "Delete", exact: true }),
    );
    expect(screen.getByRole("dialog", { name: "Delete chain?" })).toBeTruthy();
    expect(writes).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Cancel deletion" }));
    expect(writes).toHaveLength(0);
    fireEvent.click(
      screen.getByRole("button", { name: "Delete", exact: true }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Delete chain", exact: true }),
    );
    await waitFor(() =>
      expect(writes).toContainEqual({
        path: "/api/model-routing/chains/test%2Fchain",
        method: "DELETE",
        body: undefined,
      }),
    );
    await screen.findByText(/No chains configured/);
  });
  it("keeps a draft or runs the requested navigation after explicit discard", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: /Test chain test\/chain/ }),
    );
    fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Draft" },
    });
    const leave = vi.fn();
    expect(confirmLeaveRouting(leave)).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
    expect(leave).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Name")).toHaveProperty("value", "Draft");
    confirmLeaveRouting(leave);
    fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    expect(leave).toHaveBeenCalledTimes(1);
    expect(confirmLeaveRouting()).toBe(true);
  });
  it("filters fallback events and clears one account cooldown", async () => {
    await mount();
    fireEvent.change(screen.getByLabelText("Chain"), {
      target: { value: "test/chain" },
    });
    expect(screen.getByText(/→ Chain exhausted/)).toBeTruthy();
    fireEvent.click(
      screen.getAllByRole("button", {
        name: "Clear cooldown",
        hidden: true,
      })[0],
    );
    await waitFor(() =>
      expect(
        writes.some(
          (w) => w.path === "/api/model-routing/health/account-one/clear",
        ),
      ).toBe(true),
    );
  });
  it("updates the server identity when reconnecting", async () => {
    await mount();
    setConnection("https://other-routing.test", "new-test");
    await waitFor(() =>
      expect(screen.getByText(/https:\/\/other-routing.test/)).toBeTruthy(),
    );
    expect(screen.queryByText(/https:\/\/routing.test/)).toBeNull();
  });
  it("clears server data and drafts on disconnect", async () => {
    await mount();
    fireEvent.click(
      screen.getByRole("button", { name: /Test chain test\/chain/ }),
    );
    fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Old server draft" },
    });
    clearConnection();
    await screen.findByText("Connect to a backend to configure routing.");
    expect(screen.queryByText("First account")).toBeNull();
    expect(confirmLeaveRouting()).toBe(true);
  });
});
