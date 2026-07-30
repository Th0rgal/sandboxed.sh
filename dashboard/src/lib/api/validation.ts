import { apiGet, apiPost } from "./core";

export type ValidationMode = "incremental" | "clean";
export type GateStatus =
  | "pending"
  | "ready"
  | "running"
  | "passed"
  | "failed"
  | "blocked"
  | "stale";

export interface ValidationCandidate {
  repo: string;
  commit: string;
  expected_head?: string;
  source_bundle_digest?: string;
}

export interface ValidationGateSpec {
  id: string;
  description?: string;
  command: string[];
  cwd?: string;
  dependencies?: string[];
  required?: boolean;
  mode?: ValidationMode;
  reuse?: boolean;
  toolchain?: string;
  timeout_secs?: number;
  artifacts?: string[];
}

export interface ValidationMatrix {
  version: 1;
  project: string;
  profile?: string;
  gates: ValidationGateSpec[];
}

export interface ValidationGate {
  id: string;
  spec: ValidationGateSpec;
  status: GateStatus;
  outcome?: "passed" | "failed" | "blocked";
  freshness?: "exact_head" | "stale";
  validation_key: string;
  reused_receipt_id?: string;
}

export type ValidationExecutionRef =
  | { kind: "mission_run"; run_id: string; mission_id: string }
  | { kind: "workspace_job"; job_id: string; workspace_id: string }
  | { kind: "remote_job"; job_id: string; node_id: string };

export interface ValidationReceiptInput {
  execution: ValidationExecutionRef;
  exit_code?: number;
  blocked_reason?: string;
  observed_head?: string;
  /** Required for exact-head classification of dirty-overlay candidates:
   * must match the campaign candidate's source_bundle_digest. */
  source_bundle_digest?: string;
  toolchain?: string;
  environment_digest?: string;
  cache?: {
    mode?: string;
    key?: string;
    hit?: boolean;
    clean_checkout?: boolean;
  };
  artifacts?: Array<{ path: string; sha256: string; size_bytes: number }>;
  diagnostics?: string;
  started_at?: string;
  finished_at?: string;
}

export interface ValidationCampaign {
  id: string;
  project: string;
  profile?: string;
  workspace_id?: string;
  candidate: ValidationCandidate;
  candidate_id: string;
  matrix_version: number;
  status: "active" | "completed" | "failed" | "blocked" | "merged";
  certifying: boolean;
  gates: ValidationGate[];
  receipts: unknown[];
  created_at: string;
  updated_at: string;
}

export function listValidationCampaigns(): Promise<ValidationCampaign[]> {
  return apiGet("/api/validation-campaigns/", "Failed to list validation campaigns");
}

export function getValidationCampaign(id: string): Promise<ValidationCampaign> {
  return apiGet(`/api/validation-campaigns/${id}`, "Failed to load validation campaign");
}

export function createValidationCampaign(input: {
  candidate: ValidationCandidate;
  matrix: ValidationMatrix;
  workspace_id?: string;
}): Promise<ValidationCampaign> {
  return apiPost("/api/validation-campaigns/", input);
}

export function getReadyValidationGates(id: string): Promise<ValidationGate[]> {
  return apiGet(
    `/api/validation-campaigns/${id}/ready`,
    "Failed to load ready validation gates",
  );
}

export function claimValidationGate(
  campaignId: string,
  gateId: string,
  execution: ValidationExecutionRef,
): Promise<ValidationGate> {
  return apiPost(
    `/api/validation-campaigns/${campaignId}/gates/${gateId}/claim`,
    { execution },
    "Failed to claim validation gate",
  );
}

export function recordValidationReceipt(
  campaignId: string,
  gateId: string,
  receipt: ValidationReceiptInput,
): Promise<ValidationCampaign> {
  return apiPost(
    `/api/validation-campaigns/${campaignId}/gates/${gateId}/receipts`,
    receipt,
    "Failed to record validation receipt",
  );
}

export function markValidationCampaignMerged(id: string): Promise<ValidationCampaign> {
  return apiPost(
    `/api/validation-campaigns/${id}/merged`,
    undefined,
    "Campaign is not certifying",
  );
}
