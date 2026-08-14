import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig } from "swr";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import {
  getProject,
  getProjectsOverview,
  getProjectUpdates,
  postProjectAction,
} from "@/lib/api/projects";
import type {
  ProjectHealth,
  ProjectRow,
  ProjectsOverview,
  TrackHealth,
} from "@/lib/api/projects";
import { hermesChatStream, listHermesSessions } from "@/lib/api/hermes";
import ProjectsBoard, {
  unreadCountFor,
  type ProjectLastSeen,
} from "./projects-board";

vi.mock("@/lib/api/projects", () => ({
  getProjectsOverview: vi.fn(),
  getProject: vi.fn(),
  getProjectUpdates: vi.fn(),
  postProjectAction: vi.fn(),
  bindProjectConversation: vi.fn(),
  unbindProjectConversation: vi.fn(),
}));

vi.mock("@/lib/api/hermes", () => ({
  hermesChatStream: vi.fn(),
  listHermesSessions: vi.fn(),
}));

vi.mock("@/components/markdown-content", () => ({
  MarkdownContent: ({ content }: { content: string }) => <pre>{content}</pre>,
}));

const mockedOverview = vi.mocked(getProjectsOverview);
const mockedProject = vi.mocked(getProject);
const mockedListHermesSessions = vi.mocked(listHermesSessions);
const mockedUpdates = vi.mocked(getProjectUpdates);
const mockedAction = vi.mocked(postProjectAction);
const mockedChat = vi.mocked(hermesChatStream);

function project(overrides: Partial<ProjectRow>): ProjectRow {
  return {
    slug: "verity",
    bucket: "active",
    tracker: null,
    missions: [],
    latest_update: null,
    updates_count: 0,
    attention_reasons: [],
    health: health(),
    ...overrides,
  };
}

function health(overrides: Partial<ProjectHealth> = {}): ProjectHealth {
  return {
    missions: 0,
    active: 0,
    failed: 0,
    overdue: 0,
    tracks_needing_attention: 0,
    tracks: [],
    ...overrides,
  };
}

function track(overrides: Partial<TrackHealth> = {}): TrackHealth {
  return {
    track: "phase-a",
    verdict: "active",
    missions: 1,
    active: 1,
    failed: 0,
    completed: 0,
    overdue: 0,
    desired_states: {},
    last_activity_at: null,
    ...overrides,
  };
}

function overview(projects: ProjectRow[]): ProjectsOverview {
  return {
    projects,
    archived: [],
    unrouted_updates: [],
    sources: { trackers: true, hermes_db: true },
  };
}

function renderBoard() {
  return render(
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      <ProjectsBoard />
    </SWRConfig>,
  );
}

describe("ProjectsBoard", () => {
  beforeEach(() => {
    mockedOverview.mockReset();
    mockedProject.mockReset();
    mockedUpdates.mockReset();
    mockedAction.mockReset();
    mockedChat.mockReset();
    mockedUpdates.mockResolvedValue({ slug: "verity", updates: [] });
    mockedProject.mockResolvedValue({ items: [], open_decisions: [] });
  });

  afterEach(() => {
    cleanup();
  });

  test("groups projects into sections, attention first with its reasons", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({ slug: "verity" }),
        project({
          slug: "lido-audit",
          bucket: "attention",
          attention_reasons: ["blocker signalé: lease writer fantôme"],
        }),
        project({ slug: "erc", bucket: "paused" }),
      ]),
    );

    renderBoard();

    expect(await screen.findByText("verity")).toBeInTheDocument();
    expect(screen.getAllByText("lido-audit").length).toBeGreaterThan(0);
    expect(screen.getByText("erc")).toBeInTheDocument();
    // Attention project is auto-selected (first in triage order) and its
    // reasons render in the detail pane banner.
    expect(
      await screen.findAllByText(/blocker signalé: lease writer fantôme/),
    ).not.toHaveLength(0);
    const sections = screen.getAllByText(/Needs attention|Active|Paused/);
    expect(sections.length).toBeGreaterThanOrEqual(3);
  });

  test("mission rows in the detail pane link to /control", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          missions: [
            {
              id: "f98e1ee2-0000-0000-0000-000000000000",
              status: "active",
              title: "Phase 1C slice",
              updated_at: "2026-08-01T00:00:00Z",
              github_pr: null,
            },
          ],
        }),
      ]),
    );

    renderBoard();

    // Missions are behind a collapsible summary now — expand it first.
    fireEvent.click(await screen.findByRole("button", { name: /Attempts \(1\)/ }));
    const row = await screen.findByRole("link", { name: /Phase 1C slice/ });
    expect(row).toHaveAttribute(
      "href",
      "/control?mission=f98e1ee2-0000-0000-0000-000000000000",
    );
  });

  test("card and detail lead with next_action, pending decisions, and open items", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          title: "Verity",
          next_action: "rebase/repair #2332 onto main after #2333",
          pending_decisions: 1,
          mode: "active",
          health: health({
            active: 1,
            tracks_needing_attention: 1,
            tracks: [track({ track: "c5-preflight-pr2332", verdict: "failing" })],
          }),
        }),
      ]),
    );
    mockedProject.mockResolvedValue({
      project: {
        slug: "verity",
        mode: "active",
        next_action: "rebase/repair #2332 onto main after #2333",
        blocker: "source #2332 dirty",
      },
      items: [
        {
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
        },
        {
          key: "old-merged",
          kind: "track",
          open: false,
          attempts: [
            {
              id: "done-1",
              status: "completed",
              title: "merged last week",
              updated_at: "2026-08-01T00:00:00Z",
            },
          ],
        },
      ],
      open_decisions: [{ question: "merge #2332?", status: "pending_user" }],
    });

    renderBoard();

    expect(
      await screen.findAllByText("rebase/repair #2332 onto main after #2333"),
    ).not.toHaveLength(0);
    expect(screen.getAllByText(/need you/).length).toBeGreaterThan(0);
    expect(await screen.findByText(/Moving \(1\)/)).toBeInTheDocument();
    expect(screen.getByText("c5-preflight-pr2332")).toBeInTheDocument();
    expect(screen.queryByText("old-merged")).not.toBeInTheDocument();
    expect(screen.queryByText("merged last week")).not.toBeInTheDocument();
    expect(screen.getByText("source #2332 dirty")).toBeInTheDocument();
    expect(screen.getByText("merge #2332?")).toBeInTheDocument();
  });

  test("selecting a project loads its updates timeline in the detail pane", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({ slug: "verity" }),
        project({ slug: "beal" }),
      ]),
    );
    mockedUpdates.mockImplementation(async (slug: string) => ({
      slug,
      updates:
        slug === "beal"
          ? [
              {
                headline: "P_Cl promoted to proved",
                body: "**Changé :** promotion vérifiée.",
                session_id: "sess-42",
                at: "2026-08-01T07:11:00Z",
                signature: "beal",
                blocker: null,
              },
            ]
          : [],
    }));

    renderBoard();

    fireEvent.click(await screen.findByRole("button", { name: /beal/ }));

    expect(await screen.findByText("P_Cl promoted to proved")).toBeInTheDocument();
    await waitFor(() => expect(mockedUpdates).toHaveBeenCalledWith("beal", 50));
    // First update is expanded by default: body + origin session visible.
    expect(
      await screen.findByText(/Origin conversation: sess-42/),
    ).toBeInTheDocument();
  });

  test("search filter narrows the triage list", async () => {
    mockedOverview.mockResolvedValue(
      overview([project({ slug: "verity" }), project({ slug: "beal" })]),
    );

    renderBoard();
    await screen.findByRole("button", { name: /beal/ });

    fireEvent.change(screen.getByPlaceholderText("Filter…"), {
      target: { value: "ver" },
    });

    expect(
      screen.queryByRole("button", { name: /beal/ }),
    ).not.toBeInTheDocument();
    expect(screen.getAllByText("verity").length).toBeGreaterThan(0);
  });

  test("pause action posts and refreshes the overview", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));
    mockedAction.mockResolvedValue(undefined);

    renderBoard();

    fireEvent.click(await screen.findByRole("button", { name: /^Pause$/ }));
    await waitFor(() =>
      expect(mockedAction).toHaveBeenCalledWith("verity", "pause"),
    );
  });

  test("delete requires a second confirming click", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));
    mockedAction.mockResolvedValue(undefined);

    renderBoard();

    fireEvent.click(await screen.findByRole("button", { name: /Delete/ }));
    expect(mockedAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Confirm/ }));
    await waitFor(() =>
      expect(mockedAction).toHaveBeenCalledWith("verity", "delete"),
    );
  });

  test("confirmed delete revalidates the overview and the row disappears", async () => {
    // First load shows the project; the post-delete revalidation returns an
    // overview without it, so the row must leave the triage list.
    mockedOverview
      .mockResolvedValueOnce(
        overview([project({ slug: "verity" }), project({ slug: "beal" })]),
      )
      .mockResolvedValue(overview([project({ slug: "beal" })]));
    mockedAction.mockResolvedValue(undefined);

    renderBoard();

    fireEvent.click(await screen.findByRole("button", { name: /verity/ }));
    fireEvent.click(await screen.findByRole("button", { name: /Delete/ }));
    expect(mockedAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Confirm/ }));

    await waitFor(() =>
      expect(mockedAction).toHaveBeenCalledWith("verity", "delete"),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: /verity/ }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: /beal/ })).toBeInTheDocument();
  });

  test("reply composer streams into the update's origin session", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));
    mockedUpdates.mockResolvedValue({
      slug: "verity",
      updates: [
        {
          headline: "Tick",
          body: "corps",
          session_id: "sess-cron-7",
          at: "2026-08-01T07:11:00Z",
          signature: "verity",
          blocker: null,
        },
      ],
    });
    mockedChat.mockImplementation(async (_id, _msg, handlers) => {
      handlers.onDelta("Bien re");
      handlers.onCompleted?.("Bien reçu.");
    });

    renderBoard();

    const composer = await screen.findByPlaceholderText(/sess-cron-7/);
    fireEvent.change(composer, { target: { value: "continue le plan" } });
    fireEvent.click(screen.getByTitle("Send (⌘↵)"));

    await waitFor(() =>
      expect(mockedChat).toHaveBeenCalledWith(
        "sess-cron-7",
        "continue le plan",
        expect.anything(),
      ),
    );
    expect(await screen.findByText("Bien reçu.")).toBeInTheDocument();
  });
  test("links a project to the conversation its updates come from", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          latest_update: {
            headline: "Slice fermée",
            body: null,
            session_id: "api-94765bde00d93d7f",
            at: "2026-08-04T07:11:00Z",
            signature: "verity",
            blocker: null,
          },
        }),
      ]),
    );

    renderBoard();

    const link = await screen.findByTitle("Conversation");
    expect(link).toHaveAttribute(
      "href",
      "/control?session=api-94765bde00d93d7f",
    );
  });

  test("a declared binding wins over the session of the newest delivery", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          latest_update: {
            headline: "tick",
            body: null,
            // A cron tick's throwaway session: already ended, unreachable.
            session_id: "cron_e594d751447d_20260804_120931",
            at: "2026-08-04T12:09:00Z",
            signature: "verity",
            blocker: null,
          },
          conversation: {
            session_id: "20260804_103847_86ca5c",
            source: "binding",
          },
        }),
      ]),
    );

    renderBoard();

    const link = await screen.findByTitle("Conversation");
    expect(link).toHaveAttribute(
      "href",
      "/control?session=20260804_103847_86ca5c",
    );
    // Already declared: the "choose one" affordance is gone (Rebind/Unbind
    // remain, which is the point — the operator can still change their mind).
    expect(screen.queryByTitle(/Choose the conversation/i)).not.toBeInTheDocument();
  });

  test("an inferred conversation offers to be bound", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          latest_update: {
            headline: "tick",
            body: null,
            session_id: "cron_e594d751447d_20260804_120931",
            at: "2026-08-04T12:09:00Z",
            signature: "verity",
            blocker: null,
          },
          conversation: {
            session_id: "cron_e594d751447d_20260804_120931",
            source: "latest_update",
          },
        }),
      ]),
    );

    renderBoard();

    expect(
      await screen.findByTitle(/Choose the conversation/i),
    ).toBeInTheDocument();
  });

  test("binding never persists the inferred per-tick session", async () => {
    const cronSession = "cron_e594d751447d_20260804_120931";
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          latest_update: {
            headline: "tick",
            body: null,
            session_id: cronSession,
            at: "2026-08-04T12:09:00Z",
            signature: "verity",
            blocker: null,
          },
          conversation: { session_id: cronSession, source: "latest_update" },
        }),
      ]),
    );
    mockedListHermesSessions.mockResolvedValue([
      { id: cronSession, title: "tick" },
      // An ordinary session that has ENDED is just as unreachable as a cron
      // tick: binding it would point every reply at a closed thread.
      {
        id: "20260801_120000_closed",
        title: "Old thread",
        ended_at: "2026-08-02T09:00:00Z",
      },
      { id: "20260804_103847_86ca5c", title: "Verity dev #28" },
    ]);

    renderBoard();
    fireEvent.click(await screen.findByTitle(/Choose the conversation/i));

    // The per-tick session is filtered out: binding it would cement the very
    // corpse this feature exists to replace.
    await waitFor(() => {
      const values = screen
        .getAllByRole("option")
        .map((o) => (o as HTMLOptionElement).value);
      expect(values).toContain("20260804_103847_86ca5c");
    });
    const values = screen
      .getAllByRole("option")
      .map((o) => (o as HTMLOptionElement).value);
    expect(values).not.toContain(cronSession);
    expect(values).not.toContain("20260801_120000_closed");
  });

  test("offers no conversation link before a project has an update", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));

    renderBoard();

    // The slug renders in both the list row and the detail heading.
    expect(await screen.findAllByText("verity")).not.toHaveLength(0);
    expect(screen.queryByTitle("Conversation")).not.toBeInTheDocument();
  });

  test("surfaces controller mode, and renders nothing when it is absent", async () => {
    // Absence must be indistinguishable from the pre-trailer board: a
    // controller that never adopted [CTRL: …] must not gain an "unknown" chip.
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "legacy-project",
          updates_count: 3,
          latest_update: {
            headline: "Rien de neuf",
            body: null,
            session_id: "s1",
            at: new Date().toISOString(),
            signature: "legacy-project",
            blocker: null,
          },
        }),
        project({
          slug: "benchmark",
          updates_count: 2,
          latest_update: {
            headline: "Transport bloqué",
            body: null,
            session_id: "s2",
            at: new Date().toISOString(),
            signature: "benchmark",
            mode: "blocked:transport-cap",
            blocker: null,
          },
        }),
      ]),
    );

    renderBoard();

    expect(
      await screen.findAllByText("blocked: transport-cap"),
    ).not.toHaveLength(0);
    expect(screen.queryByText(/unknown/i)).not.toBeInTheDocument();
    // The legacy row gets no chip at all — absence must look like the old board.
    expect(screen.queryByTitle("active")).not.toBeInTheDocument();
  });

  test("shows the health digest when a track needs attention", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "verity",
          updates_count: 5,
          health: health({
            missions: 4,
            active: 1,
            failed: 2,
            overdue: 1,
            tracks_needing_attention: 1,
            tracks: [track({ track: "phase-b", verdict: "failing", failed: 2 })],
          }),
          latest_update: {
            headline: "un titre bien moins utile",
            body: null,
            session_id: "s3",
            at: new Date().toISOString(),
            signature: "verity",
            blocker: null,
          },
        }),
      ]),
    );

    renderBoard();

    // The digest replaces the headline precisely because it is the more
    // actionable line when a track is failing.
    expect(await screen.findByText(/2 failing/)).toBeInTheDocument();
    expect(screen.queryByText("un titre bien moins utile")).not.toBeInTheDocument();
  });

  test("counts blocked projects in the summary strip", async () => {
    mockedOverview.mockResolvedValue(
      overview([
        project({
          slug: "benchmark",
          latest_update: {
            headline: "bloqué",
            body: null,
            session_id: "s4",
            at: new Date().toISOString(),
            signature: "benchmark",
            mode: "blocked",
            blocker: null,
          },
        }),
      ]),
    );

    renderBoard();

    expect(await screen.findByText(/1 blocked/)).toBeInTheDocument();
  });
});

describe("unreadCountFor", () => {
  const seen = (
    updates_count: number,
    latest_at: string | null = null,
  ): ProjectLastSeen => ({ updates_count, latest_at });

  const proj = (updates_count: number, at: string | null) => ({
    updates_count,
    latest_update: at
      ? {
          headline: "h",
          body: null,
          session_id: "s",
          at,
          signature: "sig",
          mode: null,
          blocker: null,
        }
      : null,
  });

  test("never-opened project counts every update", () => {
    expect(unreadCountFor(proj(7, "2026-08-08T00:00:00Z"), undefined)).toBe(7);
  });

  test("delta since last seen", () => {
    expect(unreadCountFor(proj(7, null), seen(4))).toBe(3);
  });

  test("caught up means zero", () => {
    const at = "2026-08-08T00:00:00Z";
    expect(unreadCountFor(proj(4, at), seen(4, at))).toBe(0);
  });

  test("flat count with a newer latest_update still shows one", () => {
    // The updates window is rolling: the count can stay flat while newer
    // deliveries replace older ones.
    expect(
      unreadCountFor(
        proj(4, "2026-08-08T12:00:00Z"),
        seen(4, "2026-08-08T00:00:00Z"),
      ),
    ).toBe(1);
  });

  test("count shrinking (server-side trim) does not go negative", () => {
    const at = "2026-08-08T00:00:00Z";
    expect(unreadCountFor(proj(2, at), seen(10, at))).toBe(0);
  });
});
