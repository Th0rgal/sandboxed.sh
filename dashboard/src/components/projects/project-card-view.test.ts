import { describe, expect, test } from "vitest";

import type {
  ProjectDetailPayload,
  ProjectItem,
  ProjectRow,
} from "@/lib/api/projects";
import {
  cardSummary,
  viewControllerSignal,
  viewMovingItems,
  viewOpenItems,
  viewPendingDecisions,
  viewStalledItems,
} from "./project-card-view";

function row(overrides: Partial<ProjectRow> = {}): ProjectRow {
  return {
    slug: "verity",
    title: "Verity",
    bucket: "attention",
    tracker: null,
    missions: [
      {
        id: "old-completed",
        status: "completed",
        title: "merged last week",
        updated_at: "2026-08-01T00:00:00Z",
        github_pr: "lfglabs-dev/verity#2200",
      },
      {
        id: "acked",
        status: "acknowledged",
        title: "already absorbed",
        updated_at: "2026-08-10T00:00:00Z",
        github_pr: null,
      },
    ],
    latest_update: {
      headline: "[SILENT]",
      body: null,
      session_id: "s",
      at: "2026-08-14T05:50:00Z",
      signature: "verity",
      mode: "active",
      blocker: "source #2332 dirty",
    },
    updates_count: 4,
    attention_reasons: ["same state on 3 consecutive updates"],
    next_action: "rebase/repair #2332 onto main after #2333",
    mode: "active",
    pending_decisions: 1,
    health: {
      missions: 40,
      active: 2,
      failed: 12,
      overdue: 1,
      tracks_needing_attention: 2,
      tracks: [
        {
          track: "c5-preflight-pr2332",
          verdict: "failing",
          missions: 3,
          active: 1,
          failed: 2,
          completed: 8,
          overdue: 0,
          desired_states: {},
          last_activity_at: "2026-08-14T12:00:00Z",
        },
        {
          track: "core",
          verdict: "overdue",
          missions: 5,
          active: 0,
          failed: 5,
          completed: 20,
          overdue: 1,
          desired_states: {},
          last_activity_at: "2026-08-10T00:00:00Z",
        },
        {
          track: "landed-old",
          verdict: "done",
          missions: 2,
          active: 0,
          failed: 0,
          completed: 2,
          overdue: 0,
          desired_states: {},
          last_activity_at: "2026-08-01T00:00:00Z",
        },
      ],
    },
    ...overrides,
  };
}

describe("cardSummary", () => {
  test("leads with next_action, open tracks, pending decisions — not historical missions", () => {
    const summary = cardSummary(row());
    expect(summary.nextAction).toBe(
      "rebase/repair #2332 onto main after #2333",
    );
    expect(summary.headline).toBe(
      "rebase/repair #2332 onto main after #2333",
    );
    expect(summary.blocker).toBe("source #2332 dirty");
    expect(summary.pendingDecisions).toBe(1);
    expect(summary.liveAttempts).toBe(2);
    expect(summary.openTrackCount).toBe(2);
    expect(summary.openTracks.map((track) => track.key)).toEqual([
      "c5-preflight-pr2332",
      "core",
    ]);
    expect(summary.openTracks.map((track) => track.key)).not.toContain(
      "landed-old",
    );
    expect(summary.lastSignalAt).toBe("2026-08-14T05:50:00Z");
  });

  test("falls back to the attention reason when the controller left no next_action", () => {
    const summary = cardSummary(
      row({
        next_action: null,
        attention_reasons: ["controller missing"],
        health: {
          missions: 0,
          active: 0,
          failed: 0,
          overdue: 0,
          tracks_needing_attention: 0,
          tracks: [],
        },
      }),
    );
    expect(summary.nextAction).toBeNull();
    expect(summary.headline).toBe("controller missing");
  });
});

function item(overrides: Partial<ProjectItem> = {}): ProjectItem {
  return {
    key: "c5-preflight-pr2332",
    kind: "track",
    open: true,
    attempts: [
      {
        id: "live-1",
        status: "active",
        title: "repair #2332",
        updated_at: "2026-08-14T12:00:00Z",
      },
    ],
    ...overrides,
  };
}

function payload(overrides: Partial<ProjectDetailPayload> = {}): ProjectDetailPayload {
  return {
    project: {
      slug: "verity",
      mode: "active",
      next_action: "rebase/repair #2332 onto main after #2333",
      blocker: "source #2332 dirty",
      updated_at: "2026-08-14T05:50:00Z",
    },
    items: [
      item(),
      item({
        key: "landed-old",
        open: false,
        attempts: [
          {
            id: "done-1",
            status: "completed",
            title: "merged last week",
            updated_at: "2026-08-01T00:00:00Z",
          },
        ],
      }),
      item({
        key: "core",
        open: true,
        attempts: [
          {
            id: "fail-1",
            status: "failed",
            title: "review #2097",
            updated_at: "2026-08-12T00:00:00Z",
          },
        ],
      }),
    ],
    open_decisions: [
      {
        question: "relancer coldcard_skip depuis le checkpoint ?",
        status: "pending_user",
        at: "2026-08-13T20:22:14.411515235+00:00",
      },
      {
        question: "relancer coldcard_skip depuis le checkpoint ?",
        status: "pending_user",
        at: "2026-08-13T20:22:12.331480026+00:00",
      },
      { question: "merge #2332?", status: "pending_user" },
    ],
    ...overrides,
  };
}

describe("viewOpenItems / viewControllerSignal", () => {
  test("the project view inventory is open items, not historical missions", () => {
    const items = viewOpenItems(payload());
    expect(items.map((entry) => entry.key)).toEqual([
      "c5-preflight-pr2332",
      "core",
    ]);
    expect(items.every((entry) => entry.open)).toBe(true);
    expect(items.flatMap((entry) => entry.attempts).map((a) => a.status)).not.toContain(
      "completed",
    );
    expect(items.flatMap((entry) => entry.attempts).map((a) => a.status)).not.toContain(
      "acknowledged",
    );

    const signal = viewControllerSignal(payload());
    expect(signal.nextAction).toBe(
      "rebase/repair #2332 onto main after #2333",
    );
    expect(signal.blocker).toBe("source #2332 dirty");
    expect(signal.mode).toBe("active");
    expect(signal.pendingDecisions).toBe(3);

    expect(viewMovingItems(items).map((entry) => entry.key)).toEqual([
      "c5-preflight-pr2332",
    ]);
    expect(viewStalledItems(items).map((entry) => entry.key)).toEqual(["core"]);
    expect(items[0].moving).toBe(true);

    const decisions = viewPendingDecisions(payload());
    expect(decisions).toEqual([
      {
        question: "relancer coldcard_skip depuis le checkpoint ?",
        at: "2026-08-13T20:22:12.331480026+00:00",
        count: 2,
        status: "pending_user",
      },
      {
        question: "merge #2332?",
        at: null,
        count: 1,
        status: "pending_user",
      },
    ]);
  });
});
