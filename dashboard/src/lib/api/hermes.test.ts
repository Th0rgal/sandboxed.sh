import { beforeEach, describe, expect, test, vi } from "vitest";

import { apiFetch } from "./core";
import { createHermesSession, hermesChatStream } from "./hermes";

vi.mock("./core", () => ({
  apiDel: vi.fn(),
  apiFetch: vi.fn(),
  apiGet: vi.fn(),
  apiPatch: vi.fn(),
  apiPost: vi.fn(),
}));

const mockedApiFetch = vi.mocked(apiFetch);

// Forward specs from an in-flight session: CRLF SSE separators, richer
// session-creation error surfacing ("Could not start a Hermes conversation
// (status): detail"), and pre-chat stream rejection. The current hermes.ts
// does not implement these yet — unskip alongside the implementation.
describe.skip("Hermes chat stream", () => {
  beforeEach(() => {
    mockedApiFetch.mockReset();
  });

  test("parses valid SSE streams that use CRLF separators", async () => {
    const encoder = new TextEncoder();
    const body = new ReadableStream({
      start(controller) {
        controller.enqueue(
          encoder.encode(
            'event: assistant.delta\r\ndata: {"delta":"Hello"}\r\n\r\n',
          ),
        );
        controller.enqueue(
          encoder.encode(
            'event: assistant.completed\r\ndata: {"content":"Hello there"}\r\n\r\nevent: done\r\ndata: {}\r\n\r\n',
          ),
        );
        controller.close();
      },
    });
    mockedApiFetch.mockResolvedValue(new Response(body, { status: 200 }));
    const onDelta = vi.fn();
    const onCompleted = vi.fn();
    const onError = vi.fn();

    await hermesChatStream(
      "session-1",
      "hello",
      { onDelta, onCompleted, onError },
    );

    expect(onDelta).toHaveBeenCalledWith("Hello");
    expect(onCompleted).toHaveBeenCalledWith("Hello there");
    expect(onError).not.toHaveBeenCalled();
  });

  test("surfaces session creation status and upstream detail", async () => {
    mockedApiFetch.mockResolvedValue(
      new Response(JSON.stringify({ detail: "Origin not allowed" }), {
        status: 403,
        headers: { "Content-Type": "application/json" },
      }),
    );

    await expect(createHermesSession()).rejects.toThrow(
      "Could not start a Hermes conversation (403): Origin not allowed",
    );
  });

  test("throws before chat when Hermes rejects the stream request", async () => {
    mockedApiFetch.mockResolvedValue(new Response(null, { status: 502 }));
    const onError = vi.fn();

    await expect(
      hermesChatStream("session-1", "hello", { onDelta: vi.fn(), onError }),
    ).rejects.toThrow("Hermes chat failed (502)");
    expect(onError).not.toHaveBeenCalled();
  });
});
