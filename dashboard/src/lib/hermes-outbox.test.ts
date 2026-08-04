import { beforeEach, describe, expect, test } from "vitest";

import {
  getHermesOutbox,
  hermesDeliveryObserved,
  putHermesOutbox,
  removeHermesOutbox,
  type HermesOutboxEntry,
} from "./hermes-outbox";

const entry: HermesOutboxEntry = {
  id: "hermes-delivery-1",
  sessionId: "session-1",
  content: "Continue every Verity component",
  createdAt: 1_725_000_000_000,
  userOrdinal: 1,
};

describe("Hermes durable outbox", () => {
  beforeEach(() => localStorage.clear());

  test("persists and removes an outgoing prompt", () => {
    putHermesOutbox(entry);
    expect(getHermesOutbox("session-1")).toEqual([entry]);
    expect(getHermesOutbox("another-session")).toEqual([]);

    removeHermesOutbox(entry.id);
    expect(getHermesOutbox("session-1")).toEqual([]);
  });

  test("uses the resume inflight snapshot as a lost-ack proof", () => {
    expect(
      hermesDeliveryObserved(entry, {
        inflight: { user: "Continue every Verity component" },
      }),
    ).toBe(true);
  });

  test("uses the original user ordinal instead of an older duplicate", () => {
    expect(
      hermesDeliveryObserved(entry, {
        messages: [
          { role: "user", content: "Continue every Verity component" },
        ],
      }),
    ).toBe(false);

    expect(
      hermesDeliveryObserved(entry, {
        messages: [
          { role: "user", content: "Continue every Verity component" },
          { role: "user", content: "Continue every Verity component" },
        ],
      }),
    ).toBe(true);
  });
});
