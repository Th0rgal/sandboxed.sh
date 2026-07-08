/**
 * Hermes assistant chat + alerts feed API.
 *
 * Chat goes through the dashboard-authenticated proxy
 * (`/api/assistant/hermes/*` → Hermes API server); the browser never holds
 * the Hermes API key. The alerts feed reads `mission_events` via
 * `/api/control/alerts`.
 */

import { apiDel, apiFetch, apiGet, apiPatch, apiPost } from "./core";

const HERMES_PROXY = "/api/assistant/hermes";

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface HermesSession {
  id: string;
  source?: string;
  model?: string | null;
  title?: string | null;
  started_at?: string | null;
  ended_at?: string | null;
  last_active?: string | null;
  preview?: string | null;
  message_count?: number | null;
  parent_session_id?: string | null;
}

export interface HermesMessage {
  id?: number | string;
  session_id?: string;
  role: string;
  content: string | null;
  tool_call_id?: string | null;
  tool_calls?: unknown;
  tool_name?: string | null;
  timestamp?: string | null;
  reasoning?: string | null;
  reasoning_content?: string | null;
}

export async function listHermesSessions(limit = 50): Promise<HermesSession[]> {
  const res = await apiGet<{ data: HermesSession[] }>(
    `${HERMES_PROXY}/api/sessions?source=api_server&limit=${limit}`,
    "Failed to list Hermes sessions",
  );
  return res.data ?? [];
}

export async function createHermesSession(title?: string): Promise<HermesSession> {
  const res = await apiPost<{ session: HermesSession }>(
    `${HERMES_PROXY}/api/sessions`,
    title ? { title } : {},
    "Failed to create Hermes session",
  );
  return res.session;
}

export async function getHermesSessionMessages(
  sessionId: string,
): Promise<HermesMessage[]> {
  const res = await apiGet<{ data: HermesMessage[] }>(
    `${HERMES_PROXY}/api/sessions/${encodeURIComponent(sessionId)}/messages`,
    "Failed to load Hermes session",
  );
  return res.data ?? [];
}

export async function renameHermesSession(
  sessionId: string,
  title: string,
): Promise<void> {
  await apiPatch(
    `${HERMES_PROXY}/api/sessions/${encodeURIComponent(sessionId)}`,
    { title },
    "Failed to rename Hermes session",
  );
}

export async function deleteHermesSession(sessionId: string): Promise<void> {
  await apiDel(
    `${HERMES_PROXY}/api/sessions/${encodeURIComponent(sessionId)}`,
    "Failed to delete Hermes session",
  );
}

// ---------------------------------------------------------------------------
// Chat stream (named-event SSE)
// ---------------------------------------------------------------------------

export interface HermesToolEvent {
  tool_name?: string;
  preview?: string;
  args?: unknown;
}

export interface HermesChatHandlers {
  onDelta: (text: string) => void;
  /** Reasoning text (the server surfaces it as tool.progress on `_thinking`). */
  onThinking?: (text: string) => void;
  onToolStart?: (t: HermesToolEvent) => void;
  onToolComplete?: (t: HermesToolEvent) => void;
  onToolFailed?: (t: HermesToolEvent) => void;
  /** Final assistant text for the turn (authoritative, replaces deltas). */
  onCompleted?: (content: string) => void;
  onRunCompleted?: (d: { usage?: unknown }) => void;
  onError: (message: string) => void;
}

/** POST a user message to a Hermes session and stream the turn back.
 *
 * The upstream emits named SSE events (`event: assistant.delta`,
 * `tool.started`, …, `done`); aborting the fetch cancels the run
 * server-side. */
export async function hermesChatStream(
  sessionId: string,
  message: string,
  handlers: HermesChatHandlers,
  signal?: AbortSignal,
): Promise<void> {
  const res = await apiFetch(
    `${HERMES_PROXY}/api/sessions/${encodeURIComponent(sessionId)}/chat/stream`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message }),
      signal,
    },
  );
  if (!res.ok || !res.body) {
    handlers.onError(
      res.ok ? "No response stream" : `Hermes chat failed (${res.status})`,
    );
    return;
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  let sawTerminal = false;

  const handleFrame = (frame: string) => {
    let eventName = "";
    let payloadRaw = "";
    for (const line of frame.split("\n")) {
      if (line.startsWith("event:")) eventName = line.slice(6).trim();
      else if (line.startsWith("data:")) payloadRaw += line.slice(5).trim();
      // Comment lines (`: keepalive`) are ignored.
    }
    if (!eventName || !payloadRaw) return;
    let ev: Record<string, unknown>;
    try {
      ev = JSON.parse(payloadRaw);
    } catch {
      return;
    }
    switch (eventName) {
      case "assistant.delta":
        if (typeof ev.delta === "string") handlers.onDelta(ev.delta);
        break;
      case "tool.progress":
        if (ev.tool_name === "_thinking") {
          if (typeof ev.delta === "string") handlers.onThinking?.(ev.delta);
        }
        break;
      case "tool.started":
        handlers.onToolStart?.(ev as HermesToolEvent);
        break;
      case "tool.completed":
        handlers.onToolComplete?.(ev as HermesToolEvent);
        break;
      case "tool.failed":
        handlers.onToolFailed?.(ev as HermesToolEvent);
        break;
      case "assistant.completed":
        if (typeof ev.content === "string") handlers.onCompleted?.(ev.content);
        break;
      case "run.completed":
        handlers.onRunCompleted?.(ev as { usage?: unknown });
        break;
      case "error":
        sawTerminal = true;
        handlers.onError(
          typeof ev.message === "string" ? ev.message : "Hermes run failed",
        );
        break;
      case "done":
        sawTerminal = true;
        break;
    }
  };

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let sep: number;
    while ((sep = buf.indexOf("\n\n")) >= 0) {
      handleFrame(buf.slice(0, sep));
      buf = buf.slice(sep + 2);
    }
  }
  buf += decoder.decode();
  if (buf.trim()) handleFrame(buf);
  if (!sawTerminal) {
    handlers.onError("Stream ended before completion");
  }
}

// ---------------------------------------------------------------------------
// Alerts feed
// ---------------------------------------------------------------------------

export interface AlertDelivery {
  channel: string;
  status: string;
  sent_at?: string | null;
  acknowledged_at?: string | null;
  last_error?: string | null;
}

export interface AlertMissionSummary {
  title?: string | null;
  status: string;
  workspace_name?: string | null;
  awaiting_kind?: string | null;
}

export interface AlertFeedEntry {
  mission_id: string;
  status: string;
  summary: string;
  timestamp: string;
  mission?: AlertMissionSummary;
  delivery?: AlertDelivery;
}

export interface AlertsFeedResponse {
  alerts: AlertFeedEntry[];
  next_cursor: string | null;
}

export async function listAlerts(opts?: {
  statuses?: string[];
  before?: string;
  limit?: number;
}): Promise<AlertsFeedResponse> {
  const params = new URLSearchParams();
  if (opts?.statuses?.length) params.set("statuses", opts.statuses.join(","));
  if (opts?.before) params.set("before", opts.before);
  if (opts?.limit) params.set("limit", String(opts.limit));
  const qs = params.toString();
  return apiGet<AlertsFeedResponse>(
    `/api/control/alerts${qs ? `?${qs}` : ""}`,
    "Failed to load alerts feed",
  );
}
