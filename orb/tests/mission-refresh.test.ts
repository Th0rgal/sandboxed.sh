import { beforeEach, expect, it, vi } from "vitest";
vi.mock("../src/api", () => ({ listQueuedMessages: vi.fn(async () => []) }));
vi.mock("../src/stream", () => ({ getMissionEvents: vi.fn(), storedToStream: (row: unknown) => row }));
import { getMissionEvents } from "../src/stream";
import { putTranscript, refreshTranscript } from "../src/missionCache";
beforeEach(() => vi.clearAllMocks());
it("fetches new history rather than returning a cached transcript", async () => {
  putTranscript("refresh-test", { items: [], stream: [] });
  vi.mocked(getMissionEvents).mockResolvedValue([]);
  await refreshTranscript("refresh-test");
  expect(getMissionEvents).toHaveBeenCalledWith("refresh-test");
});
it("surfaces failed refresh instead of silently reporting stale data as refreshed", async () => {
  putTranscript("refresh-failure", { items: [], stream: [] });
  vi.mocked(getMissionEvents).mockRejectedValue(new Error("offline"));
  await expect(refreshTranscript("refresh-failure")).rejects.toThrow("offline");
});
