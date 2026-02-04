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
  const data = await apiGet(
    `/api/control/missions/${missionId}/automations`,
    "Failed to fetch automations"
  );
  return (data as Automation[]).map(normalizeAutomation);
}

export async function listActiveAutomations(): Promise<Automation[]> {
  const data = await apiGet(`/api/control/automations`, "Failed to fetch active automations");
  return (data as Automation[]).map(normalizeAutomation);
}

export async function createMissionAutomation(
  missionId: string,
  input: { commandName: string; intervalSeconds: number }
): Promise<Automation> {
  const res = await apiFetch(`/api/control/missions/${missionId}/automations`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      command_source: {
        type: "library",
        name: input.commandName,
      },
      trigger: {
        type: "interval",
        seconds: input.intervalSeconds,
      },
    }),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || "Failed to create automation");
  }
  const created = (await res.json()) as Automation;
  return normalizeAutomation(created);
}

export async function getAutomation(automationId: string): Promise<Automation> {
  const data = await apiGet(
    `/api/control/automations/${automationId}`,
    "Failed to fetch automation"
  );
  return normalizeAutomation(data as Automation);
}

export async function updateAutomationActive(
  automationId: string,
  active: boolean
): Promise<Automation> {
  const data = await apiPatch(
    `/api/control/automations/${automationId}`,
    { active },
    "Failed to update automation"
  );
  return normalizeAutomation(data as Automation);
}

export async function deleteAutomation(automationId: string): Promise<void> {
  await apiDel(`/api/control/automations/${automationId}`, "Failed to delete automation");
}
