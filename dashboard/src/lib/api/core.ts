/**
 * Core API utilities and base fetch helpers.
 * All other API modules import from this file.
 */

import {
  authHeader,
  clearJwt,
  getStoredJwt,
  signalAuthRequired,
} from "../auth";
import { getRuntimeApiBase } from "../settings";

// ---------------------------------------------------------------------------
// URL Builder
// ---------------------------------------------------------------------------

export function apiUrl(pathOrUrl: string): string {
  if (/^https?:\/\//i.test(pathOrUrl)) return pathOrUrl;
  const base = getRuntimeApiBase();
  const path = pathOrUrl.startsWith("/") ? pathOrUrl : `/${pathOrUrl}`;
  return `${base}${path}`;
}

// ---------------------------------------------------------------------------
// Error Utilities
// ---------------------------------------------------------------------------

export function isNetworkError(error: unknown): boolean {
  if (!error) return false;
  if (error instanceof Error) {
    const message = error.message.toLowerCase();
    return (
      message.includes("failed to fetch") ||
      message.includes("networkerror") ||
      message.includes("load failed") ||
      message.includes("network request failed") ||
      message.includes("offline")
    );
  }
  return false;
}

export class LibraryUnavailableError extends Error {
  status: number;

  constructor(message: string) {
    super(message);
    this.name = "LibraryUnavailableError";
    this.status = 503;
  }
}

/** Error carrying the HTTP status code, for callers that need to branch on it
 * (e.g. treating a 404 on DELETE as "already gone" rather than a failure). */
export class HttpStatusError extends Error {
  status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "HttpStatusError";
    this.status = status;
  }
}

export function isHttpStatusError(
  error: unknown,
  status: number,
): error is HttpStatusError {
  return error instanceof HttpStatusError && error.status === status;
}

// ---------------------------------------------------------------------------
// Base Fetch with Auth
// ---------------------------------------------------------------------------

export async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const authentication = authHeader();
  const requestJwt = authentication.Authorization?.startsWith("Bearer ")
    ? authentication.Authorization.slice("Bearer ".length)
    : null;
  const headers: Record<string, string> = {
    ...(init?.headers ? (init.headers as Record<string, string>) : {}),
    ...authentication,
  };

  const res = await fetch(apiUrl(path), { ...init, headers });
  // A delayed 401 from a request carrying an old token must not erase a token
  // issued by a newer successful login.
  if (
    res.status === 401 &&
    (getStoredJwt()?.token ?? null) === requestJwt
  ) {
    clearJwt();
    signalAuthRequired();
  }
  return res;
}

// ---------------------------------------------------------------------------
// Internal Request Helpers
// ---------------------------------------------------------------------------

export async function apiGet<T>(path: string, errorMsg: string): Promise<T> {
  const res = await apiFetch(path);
  if (!res.ok) throw new Error(errorMsg);
  return res.json();
}

export async function apiPost<T = void>(
  path: string,
  body?: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const init: RequestInit = { method: "POST" };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await apiFetch(path, init);
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

export async function apiPut<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

export async function apiPatch<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

export async function apiDel<T = void>(path: string, errorMsg = "Request failed"): Promise<T> {
  const res = await apiFetch(path, { method: "DELETE" });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

// ---------------------------------------------------------------------------
// Library-specific Helpers (handles 503 → LibraryUnavailableError)
// ---------------------------------------------------------------------------

export async function ensureLibraryResponse(
  res: Response,
  fallbackMessage: string
): Promise<Response> {
  if (res.ok) return res;
  const text = await res.text().catch(() => "");
  const contentType = res.headers.get("content-type") || "";
  const looksLikeHtml =
    contentType.includes("text/html") || /^\s*<!doctype html/i.test(text) || /^\s*<html[\s>]/i.test(text);
  const message = looksLikeHtml
    ? `${fallbackMessage}. Received an HTML page instead of API JSON; check Settings -> API URL.`
    : text || fallbackMessage;
  if (res.status === 503) {
    throw new LibraryUnavailableError(looksLikeHtml ? "Library not initialized" : text || "Library not initialized");
  }
  throw new Error(message);
}

export async function libGet<T>(path: string, errorMsg: string): Promise<T> {
  const res = await apiFetch(path);
  await ensureLibraryResponse(res, errorMsg);
  return res.json();
}

export async function libPost<T = void>(
  path: string,
  body?: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const init: RequestInit = { method: "POST" };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await apiFetch(path, init);
  await ensureLibraryResponse(res, errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

export async function libPut<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  await ensureLibraryResponse(res, errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

export async function libDel(path: string, errorMsg = "Request failed"): Promise<void> {
  const res = await apiFetch(path, { method: "DELETE" });
  await ensureLibraryResponse(res, errorMsg);
}
