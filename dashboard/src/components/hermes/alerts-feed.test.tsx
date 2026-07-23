import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SWRConfig, useSWRConfig } from "swr";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { listAlerts } from "@/lib/api";
import { AlertsFeed } from "./alerts-feed";

vi.mock("@/lib/api", () => ({
  listAlerts: vi.fn(),
}));

const mockedListAlerts = vi.mocked(listAlerts);

describe("AlertsFeed", () => {
  beforeEach(() => {
    mockedListAlerts.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  test("stops pagination after the final older page", async () => {
    mockedListAlerts
      .mockResolvedValueOnce({
        alerts: [alert("mission-1", "2026-07-10T10:00:00Z")],
        next_cursor: "cursor-1",
      })
      .mockResolvedValueOnce({
        alerts: [alert("mission-2", "2026-07-09T10:00:00Z")],
        next_cursor: null,
      });

    render(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        <AlertsFeed />
      </SWRConfig>,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Load older" }));

    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument();
    });
    expect(mockedListAlerts).toHaveBeenCalledTimes(2);
  });

  test("reopens pagination when a refresh moves the first-page boundary", async () => {
    mockedListAlerts
      .mockResolvedValueOnce({
        alerts: [alert("mission-2", "2026-07-10T10:00:00Z")],
        next_cursor: "cursor-2",
      })
      .mockResolvedValueOnce({
        alerts: [alert("mission-1", "2026-07-09T10:00:00Z")],
        next_cursor: null,
      })
      .mockResolvedValueOnce({
        alerts: [alert("mission-3", "2026-07-11T10:00:00Z")],
        next_cursor: "cursor-3",
      })
      .mockResolvedValueOnce({
        alerts: [alert("mission-2", "2026-07-10T10:00:00Z")],
        next_cursor: "cursor-2",
      });

    render(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        <RefreshAlerts />
        <AlertsFeed />
      </SWRConfig>,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Load older" }));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Refresh feed" }));
    fireEvent.click(await screen.findByRole("button", { name: "Load older" }));

    await waitFor(() => {
      expect(mockedListAlerts).toHaveBeenLastCalledWith(
        expect.objectContaining({ before: "cursor-3" }),
      );
    });
  });
});

function RefreshAlerts() {
  const { mutate } = useSWRConfig();
  return (
    <button
      type="button"
      onClick={() => void mutate(["hermes-alerts", "all"])}
    >
      Refresh feed
    </button>
  );
}

function alert(missionId: string, timestamp: string) {
  return {
    mission_id: missionId,
    status: "completed",
    summary: "Mission completed",
    timestamp,
  };
}
