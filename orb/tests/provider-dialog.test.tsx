import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { Providers } from "../src/Providers";
import { clearConnection, setConnection } from "../src/api";

afterEach(() => { clearConnection(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
it("keeps the login modal open during callback submission and exposes a rejected login", async () => {
  setConnection("http://core.test", "test-token");
  vi.spyOn(window, "open").mockReturnValue(null);
  let finish!: (response: Response) => void;
  let submissions = 0;
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    if (url.endsWith("/callback")) {
      submissions++;
      return new Promise<Response>(resolve => { finish = resolve; });
    }
    const data = url.endsWith("/providers")
      ? [{ id: "test", provider_type: "openai", name: "Test account", enabled: true, uses_oauth: true, credential_owner: "cli_proxy", status: { type: "connected" } }]
      : url.endsWith("/cli-proxy-login")
      ? { session_id: "test-login", auth_url: "https://example.test/login", flow: "redirect" }
      : { status: "pending" };
    return new Response(JSON.stringify(data));
  }));
  render(() => <Providers />);
  fireEvent.click(await screen.findByRole("button", { name: "Actions for Test account" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Re-authenticate", exact: true }));
  const input = await screen.findByLabelText("Redirect URL (http://localhost:…)");
  fireEvent.input(input, { target: { value: "http://localhost:54545/callback?code=test" } });
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.keyDown(input, { key: "Escape" });
  expect(submissions).toBe(1);
  expect((screen.getByRole("button", { name: "Close", exact: true }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Cancel", exact: true }) as HTMLButtonElement).disabled).toBe(true);
  finish(new Response(JSON.stringify({ status: "failed", message: "Callback rejected" })));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Callback rejected"));
  fireEvent.click(screen.getByRole("button", { name: "Close", exact: true }));
  expect(screen.queryByRole("dialog")).toBeNull();
});
