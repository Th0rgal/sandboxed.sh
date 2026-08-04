"use client";

/**
 * JSON-RPC 2.0 client for the Hermes gateway WebSocket, reached through the
 * backend bridge at `/api/assistant/hermes/ws` (which verifies the dashboard
 * JWT, then pumps frames to the loopback Hermes dashboard gateway `/api/ws`).
 *
 * Wire protocol (both directions, one JSON document per message):
 *   request:  { jsonrpc: "2.0", id, method, params }
 *   response: { jsonrpc: "2.0", id, result } | { jsonrpc: "2.0", id, error }
 *   event:    { jsonrpc: "2.0", method: "event",
 *               params: { type, session_id?, payload? } }
 *
 * Modeled on Hermes' own canonical TS client (apps/shared json-rpc-gateway).
 */

import { apiUrl } from "@/lib/api/core";
import { getValidJwt } from "@/lib/auth";

export type HermesGatewayState =
  "idle" | "connecting" | "open" | "closed" | "error";

export interface HermesGatewayEvent<P = unknown> {
  type: string;
  session_id?: string;
  payload?: P;
  profile?: string;
}

interface JsonRpcFrame {
  jsonrpc?: string;
  id?: number | string | null;
  method?: string;
  params?: HermesGatewayEvent;
  result?: unknown;
  error?: { code?: number; message?: string };
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

const REQUEST_TIMEOUT_MS = 120_000;
const CONNECT_TIMEOUT_MS = 15_000;

export class HermesGatewayClient {
  private ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, PendingRequest>();
  private eventHandlers = new Set<(event: HermesGatewayEvent) => void>();
  private stateHandlers = new Set<(state: HermesGatewayState) => void>();
  private state: HermesGatewayState = "idle";
  private closedByUser = false;

  getState(): HermesGatewayState {
    return this.state;
  }

  onEvent(handler: (event: HermesGatewayEvent) => void): () => void {
    this.eventHandlers.add(handler);
    return () => this.eventHandlers.delete(handler);
  }

  onState(handler: (state: HermesGatewayState) => void): () => void {
    this.stateHandlers.add(handler);
    return () => this.stateHandlers.delete(handler);
  }

  private setState(state: HermesGatewayState) {
    this.state = state;
    for (const handler of this.stateHandlers) handler(state);
  }

  /** Connect and resolve once the socket is open (the server then emits a
   * `gateway.ready` event through `onEvent`). */
  connect(): Promise<void> {
    if (this.ws?.readyState === WebSocket.OPEN) {
      return Promise.resolve();
    }
    if (this.ws?.readyState === WebSocket.CONNECTING) {
      return Promise.reject(
        new Error("Hermes gateway connection already in progress"),
      );
    }
    this.closedByUser = false;
    const jwt = getValidJwt();
    const url = apiUrl("/api/assistant/hermes/ws").replace(/^http/, "ws");
    // The JWT travels in the subprotocol list (never the URL, so it can't
    // land in access logs); the server echoes back "sandboxed". Same
    // convention as /api/monitoring/ws.
    const protocols = jwt ? ["sandboxed", `jwt.${jwt.token}`] : ["sandboxed"];

    return new Promise<void>((resolve, reject) => {
      this.setState("connecting");
      let settled = false;
      const ws = new WebSocket(url, protocols);
      this.ws = ws;
      const connectTimer = setTimeout(() => {
        if (!settled) {
          settled = true;
          reject(new Error("Hermes gateway connect timeout"));
          ws.close();
        }
      }, CONNECT_TIMEOUT_MS);

      ws.onopen = () => {
        clearTimeout(connectTimer);
        this.setState("open");
        if (!settled) {
          settled = true;
          resolve();
        }
      };
      ws.onmessage = (message) => {
        if (typeof message.data !== "string") return;
        // Frames may arrive newline-delimited; parse each line separately.
        for (const line of message.data.split("\n")) {
          const trimmed = line.trim();
          if (!trimmed) continue;
          let frame: JsonRpcFrame;
          try {
            frame = JSON.parse(trimmed) as JsonRpcFrame;
          } catch {
            continue;
          }
          this.handleFrame(frame);
        }
      };
      ws.onerror = () => {
        clearTimeout(connectTimer);
        this.setState("error");
        if (!settled) {
          settled = true;
          reject(new Error("Hermes gateway connection failed"));
        }
      };
      ws.onclose = () => {
        clearTimeout(connectTimer);
        if (this.ws === ws) this.ws = null;
        this.failAllPending(new Error("Hermes gateway connection closed"));
        this.setState(this.closedByUser ? "closed" : "error");
        if (!settled) {
          settled = true;
          reject(new Error("Hermes gateway connection closed"));
        }
      };
    });
  }

  private handleFrame(frame: JsonRpcFrame) {
    if (frame.method === "event" && frame.params) {
      for (const handler of this.eventHandlers) handler(frame.params);
      return;
    }
    if (frame.id == null) return;
    const id = typeof frame.id === "string" ? Number(frame.id) : frame.id;
    if (!Number.isFinite(id)) return;
    const pending = this.pending.get(id as number);
    if (!pending) return;
    this.pending.delete(id as number);
    clearTimeout(pending.timer);
    if (frame.error) {
      pending.reject(new Error(frame.error.message ?? "Hermes gateway error"));
    } else {
      pending.resolve(frame.result);
    }
  }

  private failAllPending(error: Error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  request<T = unknown>(
    method: string,
    params: Record<string, unknown> = {},
    timeoutMs = REQUEST_TIMEOUT_MS,
  ): Promise<T> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("Hermes gateway is not connected"));
    }
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Hermes gateway request timed out: ${method}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });
      ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  close() {
    this.closedByUser = true;
    this.failAllPending(new Error("Hermes gateway connection closed"));
    const ws = this.ws;
    this.ws = null;
    if (ws && ws.readyState !== WebSocket.CLOSED) {
      try {
        ws.close();
      } catch {
        // Already closing; nothing to release.
      }
    }
    this.setState("closed");
  }
}
