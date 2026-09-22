import { describe, it, expect, vi, beforeEach } from "vitest";
vi.mock("../src/api", () => ({ getApiUrl: () => "https://example.test", getJwt: () => "test" }));
import { readHistory, saveHistory, freshSamples } from "../src/resourceCache";
describe("resource history cache", () => {
  beforeEach(() => sessionStorage.clear());
  it("restores samples and keeps machine histories separate", () => {
    const now = Date.now();
    saveHistory("core", [{ time: now, cpu: 22 }], true);
    saveHistory("node:spark", [{ time: now, memory: 44 }], true);
    expect(readHistory("core")).toEqual([{ time: now, cpu: 22 }]);
    expect(readHistory("node:spark")).toEqual([{ time: now, memory: 44 }]);
    expect(sessionStorage.length).toBe(2);
  });
  it("expires stale data, rejects future samples and deduplicates resync", () => {
    expect(freshSamples([{ time: 1, cpu: 1 }, { time: 70000, cpu: 2 }, { time: 70000, cpu: 3 }, { time: 80000, cpu: 4 }], 70000)).toEqual([{ time: 70000, cpu: 3 }]);
  });
});
