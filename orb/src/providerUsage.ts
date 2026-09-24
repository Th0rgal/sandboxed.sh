import type { AIProvider, ProviderUsage } from "./api";

export function codexWindowLabel(minutes: number | undefined, fallback: string) {
  if (minutes == null) return fallback;
  if (minutes === 10080) return "Weekly";
  if (minutes >= 1440 && minutes % 1440 === 0) return `${minutes / 1440}d`;
  if (minutes >= 60 && minutes % 60 === 0) return `${minutes / 60}h`;
  return `${minutes}m`;
}

export function usageWindows(u: ProviderUsage) {
  const windows: { label: string; used: number }[] = [];
  if (u.unified_5h_utilization != null) windows.push({ label: "5h", used: u.unified_5h_utilization });
  if (u.unified_7d_utilization != null) windows.push({ label: "7d", used: u.unified_7d_utilization });
  if (u.codex_primary_used_percent != null && u.codex_primary_window_minutes !== 0)
    windows.push({ label: codexWindowLabel(u.codex_primary_window_minutes, "Primary"), used: u.codex_primary_used_percent / 100 });
  if (u.codex_secondary_used_percent != null && u.codex_secondary_window_minutes !== 0)
    windows.push({ label: codexWindowLabel(u.codex_secondary_window_minutes, "Secondary"), used: u.codex_secondary_used_percent / 100 });
  return windows;
}

export function effectiveProviderStatus(a: AIProvider, usage?: ProviderUsage) {
  if (a.status.type === "needs_reauth" || usage?.status === "needs_reauth") return "needs_reauth";
  if (usage?.error) return "error";
  if (usage && usageWindows(usage).some(window => window.used >= 1)) return "quota_exhausted";
  return a.status.type;
}

/** Match the data actually rendered in UsageDetail, not an empty cache object. */
export function hasProviderUsageDetails(u?: ProviderUsage): boolean {
  if (!u) return false;
  return !!(u.error || u.status === "needs_reauth" || u.account_email || u.account_name || u.organization || u.unified_status
    || (u.provider_type === "anthropic" && (u.unified_5h_utilization != null || u.unified_7d_utilization != null))
    || (u.provider_type === "openai" && ((u.codex_primary_used_percent != null && u.codex_primary_window_minutes !== 0) || u.requests_limit != null))
    || (u.provider_type === "minimax" && u.model_usage?.length)
    || (u.provider_type === "zai" && u.zai_tokens_percentage != null));
}
