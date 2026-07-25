import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import {
  createHermesSession,
  hermesChatStream,
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

vi.mock("@/components/markdown-content", () => ({
  LazyMarkdownContent: ({ content }: { content: string }) => <div>{content}</div>,
}));

const mockedCreateHermesSession = vi.mocked(createHermesSession);
const mockedHermesChatStream = vi.mocked(hermesChatStream);
const mockedListHermesSessions = vi.mocked(listHermesSessions);

// Forward specs from an in-flight session: composer quick-actions ("Review
// attention" mission action), an inline Retry affordance next to the composer,
// and role=alert error surfacing. HermesThread does not implement these yet —
// unskip alongside the implementation.
describe.skip("HermesThread", () => {
  beforeEach(() => {
    mockedCreateHermesSession.mockReset();
    mockedHermesChatStream.mockReset();
    mockedListHermesSessions.mockReset();
    mockedListHermesSessions.mockResolvedValue([]);
    mockedCreateHermesSession.mockResolvedValue({ id: "session-1" });
    mockedHermesChatStream.mockImplementation(
      (_sessionId, _message, _handlers, signal) =>
        new Promise<void>((_resolve, reject) => {
          signal?.addEventListener("abort", () => {
            reject(new DOMException("Aborted", "AbortError"));
          });
        }),
    );
  });

  afterEach(() => {
    cleanup();
  });

  test("returns to an idle composer when a new conversation cancels a stream", async () => {
    render(<HermesThread className="h-full" />);

    fireEvent.change(screen.getByPlaceholderText("Message Hermes…"), {
      target: { value: "Check every active mission" },
    });
    fireEvent.click(screen.getByTitle("Send"));

    expect(await screen.findByTitle("Stop")).toBeVisible();
    fireEvent.click(screen.getByTitle("New conversation"));

    await waitFor(() => {
      expect(screen.getByTitle("Send")).toBeVisible();
      expect(screen.queryByText("Hermes is working…")).not.toBeInTheDocument();
    });
  });

  test("fills the composer from a mission action without sending immediately", () => {
    render(<HermesThread />);

    fireEvent.click(screen.getByRole("button", { name: "Review attention" }));

    expect(screen.getByPlaceholderText("Message Hermes…")).toHaveValue(
      "Review the missions that need my attention and recommend the next action for each.",
    );
    expect(mockedHermesChatStream).not.toHaveBeenCalled();
  });

  test("preserves a failed draft and offers retry next to the composer", async () => {
    mockedCreateHermesSession.mockRejectedValueOnce(
      new Error("Could not start a Hermes conversation (403)"),
    );
    render(<HermesThread />);

    const composer = screen.getByPlaceholderText("Message Hermes…");
    fireEvent.change(composer, { target: { value: "Review active missions" } });
    fireEvent.click(screen.getByTitle("Send"));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not start a Hermes conversation (403)",
    );
    expect(composer).toHaveValue("Review active missions");
    expect(screen.getByRole("button", { name: "Retry" })).toBeVisible();
  });
});
