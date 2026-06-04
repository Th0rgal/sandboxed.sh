import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DesktopStream } from "./desktop-stream";

vi.mock("@/lib/auth", () => ({
  getValidJwt: () => null,
}));

vi.mock("@/lib/settings", () => ({
  getRuntimeApiBase: () => "http://localhost:3000",
}));

class MockWebSocket {
  static OPEN = 1;

  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  readyState = MockWebSocket.OPEN;
  sent: string[] = [];

  constructor(
    public url: string,
    public protocols: string[],
  ) {
    sockets.push(this);
  }

  send(message: string) {
    this.sent.push(message);
  }

  close() {
    this.readyState = 3;
    this.onclose?.();
  }
}

let sockets: MockWebSocket[] = [];
let originalWebSocket: typeof WebSocket;

beforeEach(() => {
  sockets = [];
  originalWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
});

afterEach(() => {
  cleanup();
  globalThis.WebSocket = originalWebSocket;
});

describe("DesktopStream", () => {
  it("renders the app-surface chrome and viewport controls", async () => {
    render(<DesktopStream displayId=":99" displayServer="wayland" compositor="sway" />);

    await waitFor(() => expect(sockets).toHaveLength(1));

    await act(async () => {
      sockets[0].onopen?.();
    });

    expect(screen.getByText("Interactive app surface")).toBeInTheDocument();
    expect(screen.getByText("Wayland app stream")).toBeInTheDocument();
    expect(screen.getByText("Pointer")).toBeInTheDocument();
    expect(screen.getByText("Keyboard")).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "Stream FPS" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "Stream quality" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Fill surface" }));
    expect(screen.getByTestId("app-stream-canvas")).toHaveClass("h-full");
  });

  it("keeps the canvas mounted for transient input errors", async () => {
    render(<DesktopStream displayId=":99" />);

    await waitFor(() => expect(sockets).toHaveLength(1));

    await act(async () => {
      sockets[0].onopen?.();
    });

    expect(screen.getByTestId("app-stream-canvas")).toBeInTheDocument();

    await act(async () => {
      sockets[0].onmessage?.({
        data: JSON.stringify({
          error: "input_failed",
          message: "xdotool failed",
        }),
      });
    });

    expect(screen.getByTestId("app-stream-canvas")).toBeInTheDocument();
    expect(screen.getByText("xdotool failed")).toBeInTheDocument();
  });

  it("shows the fatal state for capture failures", async () => {
    render(<DesktopStream displayId=":99" />);

    await waitFor(() => expect(sockets).toHaveLength(1));

    await act(async () => {
      sockets[0].onopen?.();
    });

    await act(async () => {
      sockets[0].onmessage?.({
        data: JSON.stringify({
          error: "capture_failed",
          message: "Cannot connect to display :99. The session may have ended.",
        }),
      });
    });

    expect(screen.queryByTestId("app-stream-canvas")).not.toBeInTheDocument();
    expect(screen.getByText("App Stream Unavailable")).toBeInTheDocument();
  });
});
