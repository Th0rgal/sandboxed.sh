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

for (const action of ["archive", "restore"] as const) {
  it(`${action} keeps the controller paused and preserves archival state`, async () => {
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({
      slug: "verity", job: {id: "controller", enabled: false, state: "paused", archived: action === "archive"}, runs: [],
    }), {status: 200}));
    const view = await controllerAction("verity", action);
    expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toEqual({action});
    expect(view.job?.archived).toBe(action === "archive");
    expect(view.job?.enabled).toBe(false);
  });
}
