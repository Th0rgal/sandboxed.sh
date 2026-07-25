import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import {
  getHermesSessionMessages,
  listHermesSessions,
} from "@/lib/api";
import { HermesThread } from "./hermes-thread";

vi.mock("@/lib/api", () => ({
  createHermesSession: vi.fn(),
  deleteHermesSession: vi.fn(),
  getHermesSessionMessages: vi.fn(),
  hermesChatStream: vi.fn(),
  listHermesSessions: vi.fn(),
}));

const mockedMessages = vi.mocked(getHermesSessionMessages);
const mockedSessions = vi.mocked(listHermesSessions);

describe("Hermes Desktop durable delivery", () => {
  let polls: Array<{ handler: () => void; timeout?: number }>;
  let intervalSpy: { mockRestore: () => void };

  beforeEach(() => {
    polls = [];
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
    mockedSessions.mockResolvedValue([
      { id: "api-session-1", title: "Build watcher" },
    ]);
    mockedMessages.mockResolvedValue([
      {
        id: 1,
        role: "user",
        content: "Wait until the build finishes",
      },
      {
        id: 2,
        role: "assistant",
        content: "I will report back here.",
      },
    ]);
    const spy = vi.spyOn(window, "setInterval");
    spy.mockImplementation(((
      handler: TimerHandler,
      timeout?: number,
    ) => {
      polls.push({ handler: handler as () => void, timeout });
        return 1 as unknown as ReturnType<typeof window.setInterval>;
    }) as unknown as typeof window.setInterval);
    intervalSpy = spy;
  });

  afterEach(() => {
    cleanup();
    intervalSpy.mockRestore();
    vi.clearAllMocks();
  });

  test("shows a callback appended to the active API session", async () => {
    render(<HermesThread />);

    fireEvent.click(screen.getByText("New conversation"));
    fireEvent.click(await screen.findByText("Build watcher"));

    expect(
      await screen.findByText("I will report back here."),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(polls.some(({ timeout }) => timeout === 5_000)).toBe(true),
    );

    mockedMessages.mockResolvedValue([
      {
        id: 1,
        role: "user",
        content: "Wait until the build finishes",
      },
      {
        id: 2,
        role: "assistant",
        content: "I will report back here.",
      },
      {
        id: 3,
        role: "assistant",
        content: "The build finished successfully.",
      },
    ]);

    const poll = polls.find(({ timeout }) => timeout === 5_000)?.handler;
    await act(async () => {
      poll?.();
    });
    await waitFor(() => expect(mockedMessages).toHaveBeenCalledTimes(2));

    expect(
      await screen.findByText("The build finished successfully."),
    ).toBeInTheDocument();
  });

  test("lifts the delivery prefix into a divider pill", async () => {
    mockedMessages.mockResolvedValue([
      {
        id: 1,
        role: "user",
        content: "Wait until the build finishes",
      },
      {
        id: 2,
        role: "assistant",
        content:
          "[Cron delivery: Beal roadmap progression — current Desktop session] Changement — PR #8",
      },
    ]);

    render(<HermesThread />);

    fireEvent.click(screen.getByText("New conversation"));
    fireEvent.click(await screen.findByText("Build watcher"));

    expect(
      await screen.findByText("Beal roadmap progression"),
    ).toBeInTheDocument();
    expect(await screen.findByText(/Changement — PR #8/)).toBeInTheDocument();
    expect(screen.queryByText(/\[Cron delivery:/)).not.toBeInTheDocument();
  });
});
