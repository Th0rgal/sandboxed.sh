/**
 * Remote runner nodes API — fleet status + operator cordons.
 *
 * A cordoned node keeps heartbeating and stays listed in the fleet, but is
 * excluded from automatic placement until uncordoned. The cordon set is
 * persisted server-side (`.sandboxed-sh/node_state.json`) so it survives
 * restarts.
 */

import { apiGet, apiPost } from "./core";

export interface RemoteNodeView {
  id: string;
  base_url: string;
  token_env: string;
  status: "online" | "degraded" | "offline" | "unknown" | string;
  labels: string[];
  version: string | null;
  capacity_total: number | null;
  capacity_available: number | null;
  active_jobs: number | null;
  queued_jobs: number | null;
  cpu_total: number | null;
  mem_total_bytes: number | null;
  mem_available_bytes: number | null;
  disk_total_bytes: number | null;
  disk_available_bytes: number | null;
  last_seen: string | null;
  error: string | null;
  /** Operator-cordoned: listed but excluded from automatic placement. */
  cordoned: boolean;
}

export interface RemoteNodesResponse {
  enabled: boolean;
  nodes: RemoteNodeView[];
}

export async function getRemoteNodes(): Promise<RemoteNodesResponse> {
  return apiGet("/api/remote-nodes", "Failed to fetch remote nodes");
}

export interface NodeCordonResponse {
  node: string;
  cordoned: boolean;
  changed: boolean;
}

export async function setNodeCordon(
  name: string,
  cordoned: boolean
): Promise<NodeCordonResponse> {
  const action = cordoned ? "cordon" : "uncordon";
  return apiPost(
    `/api/nodes/${encodeURIComponent(name)}/${action}`,
    {},
    `Failed to ${action} node`
  );
}
