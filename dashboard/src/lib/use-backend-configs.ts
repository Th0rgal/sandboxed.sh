import useSWR from 'swr';

import { type BackendConfig, getBackendConfig } from './api';

const DEDUPING_MS = 30000;

/**
 * Fetch persisted config for each backend id, keyed by id.
 *
 * Replaces N hand-written `useSWR('backend-X-config', ...)` calls with a
 * single SWR entry whose fetcher fans out in parallel.
 */
export function useBackendConfigs(
  ids: readonly string[]
): Record<string, BackendConfig | undefined> {
  const key = ids.length > 0 ? `backends-configs|${[...ids].sort().join(',')}` : null;
  const { data } = useSWR<Record<string, BackendConfig>>(
    key,
    async () => {
      const entries = await Promise.all(
        ids.map(async (id) => [id, await getBackendConfig(id)] as const)
      );
      return Object.fromEntries(entries);
    },
    { revalidateOnFocus: false, dedupingInterval: DEDUPING_MS }
  );
  return data ?? {};
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
