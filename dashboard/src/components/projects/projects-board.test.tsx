import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig } from "swr";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import {
  getProjectsOverview,
  getProjectUpdates,
  postProjectAction,
} from "@/lib/api/projects";
import type { ProjectRow, ProjectsOverview } from "@/lib/api/projects";
import { hermesChatStream } from "@/lib/api/hermes";
import ProjectsBoard from "./projects-board";

vi.mock("@/lib/api/projects", () => ({
  getProjectsOverview: vi.fn(),
  getProjectUpdates: vi.fn(),
  postProjectAction: vi.fn(),
}));

vi.mock("@/lib/api/hermes", () => ({
  hermesChatStream: vi.fn(),
}));

vi.mock("@/components/markdown-content", () => ({
  MarkdownContent: ({ content }: { content: string }) => <pre>{content}</pre>,
}));

const mockedOverview = vi.mocked(getProjectsOverview);
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
    mockedUpdates.mockReset();
    mockedAction.mockReset();
    mockedChat.mockReset();
    mockedUpdates.mockResolvedValue({ slug: "verity", updates: [] });
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
    fireEvent.click(await screen.findByRole("button", { name: /Missions \(1\)/ }));
    const row = await screen.findByRole("link", { name: /Phase 1C slice/ });
    expect(row).toHaveAttribute(
      "href",
      "/control?mission=f98e1ee2-0000-0000-0000-000000000000",
    );
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
      await screen.findByText(/Origin session: sess-42/),
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

  test("offers no conversation link before a project has an update", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));

    renderBoard();

    // The slug renders in both the list row and the detail heading.
    expect(await screen.findAllByText("verity")).not.toHaveLength(0);
    expect(screen.queryByTitle("Conversation")).not.toBeInTheDocument();
  });
});
