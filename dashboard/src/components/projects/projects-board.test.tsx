import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig } from "swr";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { getProjectsOverview, getProjectUpdates } from "@/lib/api/projects";
import type { ProjectRow, ProjectsOverview } from "@/lib/api/projects";
import ProjectsBoard from "./projects-board";

vi.mock("@/lib/api/projects", () => ({
  getProjectsOverview: vi.fn(),
  getProjectUpdates: vi.fn(),
}));

vi.mock("@/components/markdown-content", () => ({
  MarkdownContent: ({ content }: { content: string }) => <pre>{content}</pre>,
}));

const mockedOverview = vi.mocked(getProjectsOverview);
const mockedUpdates = vi.mocked(getProjectUpdates);

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
  });

  afterEach(() => {
    cleanup();
  });

  test("places projects in their bucket columns with attention reasons", async () => {
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
    expect(screen.getByText("lido-audit")).toBeInTheDocument();
    expect(
      screen.getByText(/blocker signalé: lease writer fantôme/),
    ).toBeInTheDocument();
    expect(screen.getByText("erc")).toBeInTheDocument();
  });

  test("mission chips link to /control", async () => {
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

    const chip = await screen.findByRole("link", { name: /Phase 1C slice/ });
    expect(chip).toHaveAttribute(
      "href",
      "/control?mission=f98e1ee2-0000-0000-0000-000000000000",
    );
  });

  test("opening a card loads the updates timeline drawer", async () => {
    mockedOverview.mockResolvedValue(overview([project({ slug: "verity" })]));
    mockedUpdates.mockResolvedValue({
      slug: "verity",
      updates: [
        {
          headline: "Phase 1C merged",
          body: "**Changé :** slice mergée.",
          session_id: "sess-42",
          at: "2026-08-01T07:11:00Z",
          signature: "verity",
          blocker: null,
        },
      ],
    });

    renderBoard();

    fireEvent.click(await screen.findByText("verity"));

    expect(await screen.findByText("Phase 1C merged")).toBeInTheDocument();
    await waitFor(() =>
      expect(mockedUpdates).toHaveBeenCalledWith("verity", 50),
    );

    fireEvent.click(screen.getByText("Phase 1C merged"));
    expect(await screen.findByText(/Session d’origine : sess-42/)).toBeInTheDocument();
  });
});
