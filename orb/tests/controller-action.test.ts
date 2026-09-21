import { afterEach, expect, it, vi } from "vitest";
import { controllerAction } from "../src/api";

afterEach(() => vi.restoreAllMocks());

it("rejects legacy success when Run now leaves the controller paused", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({
    slug: "lido", job: { id: "controller", enabled: false, state: "paused" }, runs: [],
  }), { status: 200 }));
  await expect(controllerAction("lido", "run")).rejects.toThrow("did not wake the paused controller");
});

it("accepts a queued wake without pretending the steer has been consumed", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({
    slug: "lido", job: { id: "controller", enabled: true, state: "scheduled" }, runs: [],
  }), { status: 200 }));
  const view = await controllerAction("lido", "run");
  expect(view.job?.enabled).toBe(true);
  expect(view.runs).toEqual([]);
});

it("keeps pause actions valid", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({
    slug: "lido", job: { id: "controller", enabled: false, state: "paused" }, runs: [],
  }), { status: 200 }));
  expect((await controllerAction("lido", "pause")).job?.state).toBe("paused");
});
