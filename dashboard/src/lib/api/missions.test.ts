import { describe, expect, it, vi, afterEach } from "vitest";

import { listMissions } from "./missions";

function mockFetchOnce() {
  const fetchMock = vi.fn().mockResolvedValue({
    ok: true,
    status: 200,
    json: async () => [],
    text: async () => "[]",
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function requestedUrl(fetchMock: ReturnType<typeof vi.fn>): string {
  const [input] = fetchMock.mock.calls[0];
  return typeof input === "string" ? input : String(input);
}

describe("listMissions", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps the bare path when called with no options", async () => {
    const fetchMock = mockFetchOnce();
    await listMissions();
    expect(requestedUrl(fetchMock)).toContain("/api/control/missions");
    expect(requestedUrl(fetchMock)).not.toContain("?");
  });

  it("filters by originating conversation server-side", async () => {
    const fetchMock = mockFetchOnce();
    await listMissions({ originSessionId: "20260804_103847_86ca5c", limit: 200 });
    const url = requestedUrl(fetchMock);
    expect(url).toContain("origin_session_id=20260804_103847_86ca5c");
    expect(url).toContain("limit=200");
  });

  it("sends project_prefix for a family query", async () => {
    const fetchMock = mockFetchOnce();
    await listMissions({ projectPrefix: "verity", track: "core-c3" });
    const url = requestedUrl(fetchMock);
    expect(url).toContain("project_prefix=verity");
    expect(url).toContain("track=core-c3");
  });
});
