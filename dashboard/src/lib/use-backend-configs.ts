import useSWR from 'swr';

import { type BackendConfig, getBackendConfig } from './api';

const DEDUPING_MS = 30000;

export interface BackendConfigsHandle {
  /** Latest config keyed by backend id. Missing key = not yet loaded. */
  configs: Record<string, BackendConfig | undefined>;
  /** Force a refetch of all backend configs (e.g. after a save). */
  refresh: () => Promise<void>;
}

/**
 * Fetch persisted config for every backend id in one SWR entry.
 *
 * Replaces N hand-written `useSWR('backend-X-config', ...)` calls. The
 * fetcher fans out in parallel; the cache key is derived from the id list
 * so different callers can share results.
 */
export function useBackendConfigs(ids: readonly string[]): BackendConfigsHandle {
  const key = ids.length > 0 ? `backends-configs|${[...ids].sort().join(',')}` : null;
  const { data, mutate } = useSWR<Record<string, BackendConfig>>(
    key,
    async () => {
      const entries = await Promise.all(
        ids.map(async (id) => [id, await getBackendConfig(id)] as const)
      );
      return Object.fromEntries(entries);
    },
    { revalidateOnFocus: false, dedupingInterval: DEDUPING_MS }
  );
  return {
    configs: data ?? {},
    refresh: async () => {
      await mutate();
    },
  };
}

/**
 * True when the backend has not been explicitly disabled, its CLI is reachable,
 * and (when reported) its authentication is configured.
 *
 * Returns true when the config is still loading (optimistic) so the UI doesn't
 * flicker the backend out on first paint.
 */
export function isBackendAvailable(config: BackendConfig | undefined): boolean {
  if (!config) return true;
  if (config.enabled === false) return false;
  if (config.cli_available === false) return false;
  if (config.auth_configured === false) return false;
  return true;
}
