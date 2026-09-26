import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { Providers } from "../src/Providers";
import { clearConnection, setConnection } from "../src/api";

afterEach(() => { clearConnection(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
it("reconnects a revoked sandboxed-owned Anthropic account using its existing identity", async () => {
  setConnection("http://core.test", "test-token");
  vi.spyOn(window, "open").mockReturnValue(null);
  let connected = false;
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    if (url.endsWith("/oauth/callback")) {
      expect(url).toContain("/expired-account/");
      expect(JSON.parse(options!.body as string)).toEqual({ method_index: 0, code: "authorized-code" });
      connected = true;
      return new Response(JSON.stringify({ status: { type: "connected" } }));
    }
    const data = url.endsWith("/providers") ? [{ id: "expired-account", provider_type: "anthropic", name: "Claude account", uses_oauth: true, credential_owner: "sandboxed_sh", account_email: "account@example.test", status: { type: connected ? "connected" : "needs_reauth", reason: connected ? undefined : "Refresh token revoked" } }]
      : url.endsWith("/oauth/authorize") ? { url: "https://example.test/authorize", method: "code", instructions: "Paste the authorization code." }
      : {};
    return new Response(JSON.stringify(data));
  });
  vi.stubGlobal("fetch", fetch);
  render(() => <Providers />);
  fireEvent.click(await screen.findByRole("button", { name: /^Claude account/ }));
  fireEvent.click(screen.getByRole("button", { name: "Reconnect", exact: true }));
  const input = await screen.findByLabelText("Authorization code or redirect URL");
  expect(screen.getByText(/Sign in as account@example.test/)).toBeTruthy();
  fireEvent.input(input, { target: { value: "authorized-code" } });
  fireEvent.click(screen.getByRole("button", { name: "Submit callback" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await screen.findByText("Connected");
  expect(fetch.mock.calls.some(([url]) => url.includes("cli-proxy-login"))).toBe(false);
});
it("does not expose empty API key rows as expandable buttons but keeps real error details", async () => {
  setConnection("http://core.test", "test-token");
  vi.stubGlobal("fetch", vi.fn(async (url: string) => new Response(JSON.stringify(url.endsWith("/providers")
    ? ["muse", "custom", "minimax", "zai"].map(id => ({ id, name: id, provider_type: id, uses_oauth: false, status: { type: "connected" } }))
    : { entries: { muse: { provider_type: "muse" }, custom: { provider_type: "custom" }, minimax: { provider_type: "minimax", model_usage: [] }, zai: { provider_type: "zai", error: "Account unavailable" } } }))));
  const { container } = render(() => <Providers />);
  await screen.findAllByText("muse");
  await waitFor(() => expect(screen.getByRole("button", { name: /^zai/ })).toBeTruthy());
  for (const name of ["muse", "custom", "minimax"]) expect(screen.queryByRole("button", { name: new RegExp(`^${name}`) })).toBeNull();
  expect(container.querySelectorAll(".p-acc-chev")).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: /^zai/ }));
  expect(screen.getByText("Account unavailable")).toBeTruthy();
});
