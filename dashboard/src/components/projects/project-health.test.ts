import { describe, expect, test } from "vitest";

import type { ProjectRow } from "@/lib/api/projects";
import { isStale, parseMode } from "./project-health";

function row(overrides: Partial<ProjectRow>): ProjectRow {
  return {
    slug: "verity",
    bucket: "active",
    tracker: null,
    missions: [],
    latest_update: null,
    updates_count: 0,
    attention_reasons: [],
    health: {
      missions: 0,
      active: 0,
      failed: 0,
      overdue: 0,
      tracks_needing_attention: 0,
      tracks: [],
    },
    ...overrides,
  };
}

describe("parseMode", () => {
  test("prefers the roster mode over a silent delivery", () => {
    const mode = parseMode(
      row({
        mode: "blocked:scanner-dead",
        latest_update: {
          headline: "[SILENT]",
          body: null,
          session_id: "s",
          at: "2026-08-13T20:00:00Z",
          signature: "verity",
          mode: "active",
          blocker: null,
        },
      }),
    );
    expect(mode).toEqual({ base: "blocked", cause: "scanner-dead" });
  });

  test("renders a parked-decision mode as blocked:decision", () => {
    const mode = parseMode(row({ mode: "blocked:decision" }));
    expect(mode).toEqual({ base: "blocked", cause: "decision" });
  });

  test("falls back to the delivery trailer when the roster has no mode", () => {
    const mode = parseMode(
      row({
        latest_update: {
          headline: "working",
          body: null,
          session_id: "s",
          at: "2026-08-13T20:00:00Z",
          signature: "verity",
          mode: "paused:waiting-runner",
          blocker: null,
        },
      }),
    );
    expect(mode).toEqual({ base: "paused", cause: "waiting-runner" });
  });
});

describe("isStale", () => {
  test("an active project with no delivery at all is stale", () => {
    expect(isStale(row({ bucket: "active", latest_update: null }))).toBe(true);
  });

  test("a paused project with no delivery is not flagged silent", () => {
    expect(isStale(row({ bucket: "paused", latest_update: null }))).toBe(false);
  });

  test("trusts controller_health=stale from the store", () => {
    expect(
      isStale(
        row({
          latest_update: {
            headline: "ok",
            body: null,
            session_id: "s",
            at: new Date().toISOString(),
            signature: "verity",
            mode: "active",
            blocker: null,
          },
          controller_health: "stale",
        }),
      ),
    ).toBe(true);
  });
});
