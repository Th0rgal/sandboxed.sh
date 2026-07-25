import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { SWRConfig } from "swr";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { getHermesAssistantStatus, getHermesMissionControl } from "@/lib/api";
import HermesPage from "./hermes-page";

vi.mock("@/lib/api", () => ({
  getHermesAssistantStatus: vi.fn(),
  getHermesMissionControl: vi.fn(),
}));

vi.mock("@/components/hermes/hermes-thread", () => ({
  HermesThread: () => <div data-testid="hermes-thread">conversation</div>,
}));

vi.mock("@/components/hermes/alerts-feed", () => ({
  AlertsFeed: () => <div>updates</div>,
}));

const mockedStatus = vi.mocked(getHermesAssistantStatus);
const mockedControl = vi.mocked(getHermesMissionControl);

describe("HermesPage", () => {
  beforeEach(() => {
    mockedStatus.mockReset();
    mockedControl.mockReset();
    mockedStatus.mockResolvedValue({
      service_name: "hermes-assistant",
      service_active: true,
      model: "builtin/smart",
      env_path: "",
      config_path: "",
      env_present: true,
      config_present: true,
      token_present: true,
      telegram_ok: true,
      telegram_bot_username: null,
      telegram_webhook_configured: false,
      telegram_pending_update_count: 0,
      telegram_last_error: null,
      notes: [],
    });
    mockedControl.mockResolvedValue({
      generated_at: "2026-07-10T10:00:00Z",
      runtime: {
        service_name: "hermes-assistant",
        service_active: true,
        model: "builtin/smart",
        base_url: "http://localhost:3000/v1",
        expected_base_url: "http://localhost:3000/v1",
        uses_sandboxed_proxy: true,
        env_present: true,
        config_present: true,
        token_present: true,
        notes: [],
      },
      sessions: {
        since: "2026-07-07T10:00:00Z",
        total: 2,
        by_source: { api_server: 2 },
        messages: 10,
        tool_calls: 3,
        open: 1,
      },
      active: [],
      needs_attention: [],
      handled_recently: [],
      failures: [],
      mission_status_counts: {},
      remote_nodes: {
        enabled: false,
        configured_nodes: 0,
        status: "disabled",
        notes: [],
      },
    });
  });

  afterEach(() => {
    cleanup();
  });

  test("keeps conversation primary and mission control closed by default", async () => {
    render(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        <HermesPage />
      </SWRConfig>,
    );

    expect(await screen.findByTestId("hermes-thread")).toBeVisible();
    expect(screen.queryByRole("dialog", { name: "Mission control" })).not.toBeInTheDocument();

    const missionsButton = screen.getByRole("button", { name: "Missions" });
    missionsButton.focus();
    fireEvent.click(missionsButton);
    expect(screen.getByRole("dialog", { name: "Mission control" })).toBeVisible();

    fireEvent.click(missionsButton);
    expect(screen.queryByRole("dialog", { name: "Mission control" })).not.toBeInTheDocument();

    missionsButton.focus();
    fireEvent.click(missionsButton);
    expect(screen.getByRole("dialog", { name: "Mission control" })).toBeVisible();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Mission control" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Missions" })).toHaveFocus();
  });
});
