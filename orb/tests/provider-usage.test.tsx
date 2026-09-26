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
it("shows the xAI subscription quota and plan and detects exhaustion", async () => {
  setConnection("http://core.test", "test-token");
  const xai = {...account("grok"), provider_type:"xai"};
  const quota:ProviderUsage = {provider_type:"xai",xai_plan:"SuperGrok",xai_credit_label:"Weekly",xai_credit_used_percent:42,xai_credit_reset:1900000000};
  vi.stubGlobal("fetch", vi.fn(async (url:string)=>new Response(JSON.stringify(url.endsWith("/providers")?[xai]:url.includes("/grok/usage")?quota:{}))));
  const {container}=render(()=><Providers/>);
  await waitFor(()=>expect(container.textContent).toContain("42%"));
  expect(container.textContent).toContain("SuperGrok plan");
  expect(usageWindows(quota)).toEqual([{label:"Weekly",used:.42}]);
  expect(effectiveProviderStatus(xai,{...quota,xai_credit_used_percent:100})).toBe("quota_exhausted");
  expect(usageWindows({provider_type:"xai"})).toEqual([]);
});
it("loads Kimi subscription-key quota on first visit and avoids duplicate windows", async () => {
  setConnection("http://core.test", "test-token");
  const kimi = {...account("kimi-account"),provider_type:"kimi",uses_oauth:false};
  const quota:ProviderUsage = {provider_type:"kimi",kimi_plan:"Moderato",kimi_5h_used_percent:0,kimi_weekly_used_percent:75,kimi_windows:[{label:"5h",used_percent:0,reset_at:1900000000},{label:"Weekly",used_percent:75}]};
  vi.stubGlobal("fetch",vi.fn(async(url:string)=>new Response(JSON.stringify(url.endsWith("/providers")?[kimi]:url.includes("/kimi-account/usage")?quota:{}))));
  const {container}=render(()=><Providers/>);
  await waitFor(()=>expect(container.textContent).toContain("75%"));
  expect(container.textContent).toContain("Moderato plan");
  expect(usageWindows(quota)).toEqual([{label:"5h",used:0},{label:"Weekly",used:.75}]);
  expect(usageWindows({...quota,kimi_windows:undefined})).toEqual(usageWindows(quota));
  expect(effectiveProviderStatus(kimi,{provider_type:"kimi",kimi_weekly_used_percent:100})).toBe("quota_exhausted");
  expect(usageWindows({provider_type:"kimi"})).toEqual([]);
});

it("loads API-key coding plans on the first visit with provider-specific quota semantics", async () => {
  setConnection("http://core.test", "test-token");
  const accounts = ["minimax", "zai"].map(type => ({...account(type),provider_type:type,uses_oauth:false}));
  const mini: ProviderUsage = {provider_type:"minimax",minimax_interval_remaining_percent:80,minimax_weekly_remaining_percent:0};
  const zai: ProviderUsage = {provider_type:"zai",zai_5h_used_percent:1,zai_weekly_used_percent:75};
  vi.stubGlobal("fetch",vi.fn(async(url:string)=>new Response(JSON.stringify(url.endsWith("/providers")?accounts:url.includes("/minimax/usage")?mini:url.includes("/zai/usage")?zai:{}))));
  const {container}=render(()=><Providers/>);
  await waitFor(()=>expect(container.textContent).toContain("75%"));
  expect(container.textContent).toContain("20%");
  expect(container.textContent).toContain("Quota exhausted");
  expect(usageWindows(mini)).toEqual([{label:"5h",used:.2},{label:"Weekly",used:1}]);
  expect(usageWindows(zai)).toEqual([{label:"5h",used:.01},{label:"Weekly",used:.75}]);
  expect(usageWindows({provider_type:"minimax"})).toEqual([]);
});
it("does not mistake absent subscription data for an exhausted account", () => {
  const u:ProviderUsage={provider_type:"minimax",usage_note:"No active Token Plan subscription for this API key."};
  expect(usageWindows(u)).toEqual([]);
  expect(effectiveProviderStatus({...account("mini"),provider_type:"minimax"},u)).toBe("connected");
});
