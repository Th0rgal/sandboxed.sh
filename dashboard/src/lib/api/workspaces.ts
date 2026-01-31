/**
 * Workspaces API - CRUD operations for workspaces.
 */

import { apiGet, apiPost, apiPut, apiDel, apiFetch } from "./core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type WorkspaceType = "host" | "container";
export type WorkspaceStatus = "pending" | "building" | "ready" | "error";
export type TailscaleMode = "exit_node" | "tailnet_only";

export interface Workspace {
  id: string;
  name: string;
  workspace_type: WorkspaceType;
  path: string;
  status: WorkspaceStatus;
  error_message: string | null;
  created_at: string;
  skills: string[];
  plugins: string[];
  template?: string | null;
  distro?: string | null;
  env_vars: Record<string, string>;
  init_script?: string | null;
  shared_network?: boolean | null;
  tailscale_mode?: TailscaleMode | null;
}

export type ContainerDistro =
  | "ubuntu-noble"
  | "ubuntu-jammy"
  | "debian-bookworm"
  | "arch-linux";

export const CONTAINER_DISTROS: { value: ContainerDistro; label: string }[] = [
  { value: "ubuntu-noble", label: "Ubuntu 24.04 LTS (Noble)" },
  { value: "ubuntu-jammy", label: "Ubuntu 22.04 LTS (Jammy)" },
  { value: "debian-bookworm", label: "Debian 12 (Bookworm)" },
  { value: "arch-linux", label: "Arch Linux (Base)" },
];

export interface WorkspaceDebugInfo {
  id: string;
  name: string;
  status: string;
  path: string;
  path_exists: boolean;
  size_bytes: number | null;
  directories: { path: string; exists: boolean; file_count: number | null }[];
  has_bash: boolean;
  init_script_exists: boolean;
  init_script_modified: string | null;
  distro: string | null;
  last_error: string | null;
}

export interface InitLogResponse {
  exists: boolean;
  content: string | null;
  total_lines: number | null;
  log_path: string;
}

// ---------------------------------------------------------------------------
// API Functions
// ---------------------------------------------------------------------------

export async function listWorkspaces(): Promise<Workspace[]> {
  return apiGet("/api/workspaces", "Failed to fetch workspaces");
}

export async function getWorkspace(id: string): Promise<Workspace> {
  return apiGet(`/api/workspaces/${id}`, "Failed to fetch workspace");
}

export async function createWorkspace(data: {
  name: string;
  workspace_type: WorkspaceType;
  path?: string;
  skills?: string[];
  plugins?: string[];
  template?: string;
  distro?: string;
  env_vars?: Record<string, string>;
  init_script?: string;
  shared_network?: boolean | null;
  tailscale_mode?: TailscaleMode | null;
}): Promise<Workspace> {
  return apiPost("/api/workspaces", data, "Failed to create workspace");
}

export async function updateWorkspace(
  id: string,
  data: {
    name?: string;
    skills?: string[];
    plugins?: string[];
    template?: string | null;
    distro?: string | null;
    env_vars?: Record<string, string>;
    init_script?: string | null;
    shared_network?: boolean | null;
    tailscale_mode?: TailscaleMode | null;
  }
): Promise<Workspace> {
  return apiPut(`/api/workspaces/${id}`, data, "Failed to update workspace");
}

export async function syncWorkspace(id: string): Promise<Workspace> {
  return apiPost(`/api/workspaces/${id}/sync`, undefined, "Failed to sync workspace");
}

export async function deleteWorkspace(id: string): Promise<void> {
  return apiDel(`/api/workspaces/${id}`, "Failed to delete workspace");
}

export async function buildWorkspace(
  id: string,
  distro?: ContainerDistro,
  rebuild?: boolean
): Promise<Workspace> {
  const res = await apiFetch(`/api/workspaces/${id}/build`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: distro || rebuild ? JSON.stringify({ distro, rebuild }) : undefined,
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to build workspace");
  }
  return res.json();
}

export async function getWorkspaceDebug(id: string): Promise<WorkspaceDebugInfo> {
  const res = await apiFetch(`/api/workspaces/${id}/debug`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to get workspace debug info");
  }
  return res.json();
}

export async function getWorkspaceInitLog(id: string): Promise<InitLogResponse> {
  const res = await apiFetch(`/api/workspaces/${id}/init-log`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to get init log");
  }
  return res.json();
}
