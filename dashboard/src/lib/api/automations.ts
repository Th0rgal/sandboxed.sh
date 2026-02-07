/**
 * Automations API - scheduled command triggers for missions.
 */

import { apiFetch, apiGet, apiPatch, apiDel } from "./core";

export type CommandSource =
  | { type: "library"; name: string }
  | { type: "local_file"; path: string }
  | { type: "inline"; content: string };

export type TriggerType =
  | { type: "interval"; seconds: number }
  | {
      type: "webhook";
      config: {
        webhook_id: string;
        secret?: string | null;
        variable_mappings?: Record<string, string>;
      };
    };

export interface Automation {
  id: string;
  mission_id: string;
  command_source: CommandSource;
  trigger: TriggerType;
  variables?: Record<string, string>;
  active: boolean;
  created_at: string;
  last_triggered_at?: string | null;
  retry_config?: {
    max_retries: number;
    retry_delay_seconds: number;
    backoff_multiplier: number;
  };
  // Back-compat fields used by the UI
  command_name?: string;
  interval_seconds?: number;
}

export type ExecutionStatus =
  | "pending"
  | "running"
  | "success"
  | "failed"
  | "cancelled"
  | "skipped";

export interface AutomationExecution {
  id: string;
  automation_id: string;
  mission_id: string;
  triggered_at: string;
  trigger_source: string;
  status: ExecutionStatus;
  webhook_payload?: unknown;
  variables_used: Record<string, string>;
  completed_at?: string | null;
  error?: string | null;
  retry_count: number;
}

export interface CreateAutomationInput {
  command_source: CommandSource;
  trigger: TriggerType;
  variables?: Record<string, string>;
}

function normalizeAutomation(raw: Automation): Automation {
  const command_name =
    raw.command_source?.type === "library" ? raw.command_source.name : undefined;
  const interval_seconds =
    raw.trigger?.type === "interval" ? raw.trigger.seconds : undefined;
  return {
    ...raw,
    command_name,
    interval_seconds,
  };
}

export async function listMissionAutomations(missionId: string): Promise<Automation[]> {
  const data = await apiGet<Automation[]>(
    `/api/control/missions/${missionId}/automations`,
    "Failed to fetch automations"
  );
  return data.map(normalizeAutomation);
}

export async function listActiveAutomations(): Promise<Automation[]> {
  const data = await apiGet<Automation[]>(
    `/api/control/automations`,
    "Failed to fetch active automations"
  );
  return data.map(normalizeAutomation);
}

export async function createMissionAutomation(
  missionId: string,
  input: CreateAutomationInput
): Promise<Automation> {
  const res = await apiFetch(`/api/control/missions/${missionId}/automations`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || "Failed to create automation");
  }
  const created = (await res.json()) as Automation;
  return normalizeAutomation(created);
}

export async function getAutomation(automationId: string): Promise<Automation> {
  const data = await apiGet<Automation>(
    `/api/control/automations/${automationId}`,
    "Failed to fetch automation"
  );
  return normalizeAutomation(data);
}

export async function updateAutomation(
  automationId: string,
  updates: {
    command_source?: CommandSource;
    trigger?: TriggerType;
    variables?: Record<string, string>;
    active?: boolean;
  }
): Promise<Automation> {
  const data = await apiPatch<Automation>(
    `/api/control/automations/${automationId}`,
    updates,
    "Failed to update automation"
  );
  return normalizeAutomation(data);
}

/** @deprecated Use updateAutomation instead */
export async function updateAutomationActive(
  automationId: string,
  active: boolean
): Promise<Automation> {
  return updateAutomation(automationId, { active });
}

export async function deleteAutomation(automationId: string): Promise<void> {
  await apiDel(`/api/control/automations/${automationId}`, "Failed to delete automation");
}

export async function getAutomationExecutions(
  automationId: string
): Promise<AutomationExecution[]> {
  return apiGet<AutomationExecution[]>(
    `/api/control/automations/${automationId}/executions`,
    "Failed to fetch automation executions"
  );
}

export async function getMissionAutomationExecutions(
  missionId: string
): Promise<AutomationExecution[]> {
  return apiGet<AutomationExecution[]>(
    `/api/control/missions/${missionId}/automation-executions`,
    "Failed to fetch mission automation executions"
  );
}
