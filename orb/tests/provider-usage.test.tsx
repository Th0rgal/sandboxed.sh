import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { Providers } from "../src/Providers";
import { clearConnection, setConnection, type AIProvider, type ProviderUsage } from "../src/api";
import { effectiveProviderStatus, usageWindows } from "../src/providerUsage";
const account = (id: string) => ({ id, provider_type: "openai", name: id, enabled: true, uses_oauth: true, credential_owner: "sandboxed_sh", status: { type: "connected" } }) as AIProvider;
const usage = (used: number) => ({ provider_type: "openai", codex_primary_used_percent: used, codex_primary_window_minutes: 10080, codex_secondary_used_percent: 0, codex_secondary_window_minutes: 0 }) as ProviderUsage;
afterEach(() => { clearConnection(); vi.unstubAllGlobals(); });
it("shows both account quotas on the first visit even when the bulk cache is empty", async () => {
  setConnection("http://core.test", "test-token");
  vi.stubGlobal("fetch", vi.fn(async (url: string) => new Response(JSON.stringify(url.endsWith("/providers") ? [account("ben"), account("thomas")] : url.includes("/ben/usage") ? usage(45) : url.includes("/thomas/usage") ? usage(100) : {}))));
  const { container } = render(() => <Providers />);
  await waitFor(() => expect(container.textContent).toContain("45%"));
  expect(container.textContent).toContain("100%");
  expect(container.textContent).toContain("Quota exhausted");
  expect(container.textContent).toContain("Weekly");
  expect(container.textContent).not.toContain("5h");
});
it("uses actual window lengths and excludes absent zero-duration windows", () => {
  expect(usageWindows(usage(45))).toEqual([{ label: "Weekly", used: .45 }]);
  expect(usageWindows({ ...usage(45), codex_primary_window_minutes: 300 })).toEqual([{ label: "5h", used: .45 }]);
});
it("does not equate saved credentials with usable quota or a working login", () => {
  expect(effectiveProviderStatus(account("ben"), usage(45))).toBe("connected");
  expect(effectiveProviderStatus(account("thomas"), usage(100))).toBe("quota_exhausted");
  expect(effectiveProviderStatus(account("ben"), { ...usage(45), status: "needs_reauth" })).toBe("needs_reauth");
});
