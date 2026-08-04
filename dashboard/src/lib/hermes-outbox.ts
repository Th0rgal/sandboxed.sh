import type { HermesMessage } from "@/lib/api";

const STORAGE_KEY = "sandboxed-hermes-outbox-v1";
const MAX_ENTRIES = 50;

export interface HermesOutboxEntry {
  id: string;
  sessionId: string;
  content: string;
  createdAt: number;
  /** Number of durable user messages visible before this send. */
  userOrdinal: number;
}

export interface HermesResumeEvidence {
  messages?: HermesMessage[];
  inflight?: { user?: string | null } | null;
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function validEntry(value: unknown): value is HermesOutboxEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as Partial<HermesOutboxEntry>;
  return (
    typeof entry.id === "string" &&
    typeof entry.sessionId === "string" &&
    typeof entry.content === "string" &&
    typeof entry.createdAt === "number" &&
    typeof entry.userOrdinal === "number"
  );
}

function readAll(): HermesOutboxEntry[] {
  const target = storage();
  if (!target) return [];
  try {
    const parsed = JSON.parse(target.getItem(STORAGE_KEY) ?? "[]") as unknown;
    return Array.isArray(parsed) ? parsed.filter(validEntry) : [];
  } catch {
    return [];
  }
}

function writeAll(entries: HermesOutboxEntry[]) {
  const target = storage();
  if (!target) return;
  try {
    target.setItem(STORAGE_KEY, JSON.stringify(entries.slice(-MAX_ENTRIES)));
  } catch {
    // Storage can be unavailable in private browsing. The visible failed
    // bubble remains the non-durable fallback.
  }
}

export function createHermesDeliveryId(): string {
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.randomUUID === "function"
  ) {
    return `hermes-${crypto.randomUUID()}`;
  }
  return `hermes-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function putHermesOutbox(entry: HermesOutboxEntry) {
  const entries = readAll().filter((candidate) => candidate.id !== entry.id);
  entries.push(entry);
  writeAll(entries);
}

export function removeHermesOutbox(id: string) {
  writeAll(readAll().filter((entry) => entry.id !== id));
}

export function getHermesOutbox(sessionId: string): HermesOutboxEntry[] {
  return readAll().filter((entry) => entry.sessionId === sessionId);
}

function normalize(text: string | null | undefined): string {
  return (text ?? "").trim().replace(/\r\n/g, "\n");
}

/**
 * Prove that Hermes accepted an outbox entry before retrying it.
 *
 * `prompt.submit` records `inflight.user` before returning its JSON-RPC ACK.
 * If that ACK is lost with the socket, `session.resume` replays the inflight
 * snapshot. Once the turn is durable, the user ordinal proves the same thing
 * from persisted history. This closes the ambiguous-ACK window without ever
 * submitting the prompt twice.
 */
export function hermesDeliveryObserved(
  entry: HermesOutboxEntry,
  evidence: HermesResumeEvidence,
): boolean {
  const expected = normalize(entry.content);
  if (expected && normalize(evidence.inflight?.user) === expected) return true;

  const users = (evidence.messages ?? []).filter(
    (message) => message.role === "user" && normalize(message.content),
  );
  return normalize(users[entry.userOrdinal]?.content) === expected;
}
