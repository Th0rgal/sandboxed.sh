import { afterEach, expect, it, vi } from "vitest";
import { clearConnection, setConnection } from "../src/api";
import { DEFAULT_PROJECT, ensureDefaultProject, projectChoices } from "../src/defaultProject";

afterEach(() => { clearConnection(); vi.unstubAllGlobals(); });
it("pins Default once, preserves an existing title, and sorts the remaining projects", () => {
  expect(projectChoices([])).toEqual([{ id: "default", name: "Default" }]);
  expect(projectChoices([{ slug: "old", updated_at: "2020" }, { slug: "default", title: "My inbox" }, { slug: "new", updated_at: "2026" }])).toEqual([
    { id: "default", name: "My inbox" }, { id: "new", name: "new" }, { id: "old", name: "old" },
  ]);
});
it("creates the catch-all only when it is missing and never rewrites an existing record", async () => {
  setConnection("http://core.test", "test");
  const fetcher = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ projects: [] })))
    .mockResolvedValueOnce(new Response(JSON.stringify(DEFAULT_PROJECT)))
    .mockResolvedValueOnce(new Response(JSON.stringify({ projects: [{ ...DEFAULT_PROJECT, title: "My inbox", objective: "Keep this" }] })));
  vi.stubGlobal("fetch", fetcher);
  expect(await ensureDefaultProject()).toEqual(DEFAULT_PROJECT);
  expect(JSON.parse(fetcher.mock.calls[1][1].body)).toEqual(DEFAULT_PROJECT);
  expect(await ensureDefaultProject()).toMatchObject({ title: "My inbox", objective: "Keep this" });
  expect(fetcher).toHaveBeenCalledTimes(3);
});
it("does not write after a failed read or a backend switch", async () => {
  setConnection("http://core.test", "test");
  const fetcher = vi.fn().mockResolvedValueOnce(new Response("Unavailable", { status: 503 }));
  vi.stubGlobal("fetch", fetcher);
  await expect(ensureDefaultProject()).rejects.toThrow("503");
  expect(fetcher).toHaveBeenCalledTimes(1);
  fetcher.mockImplementationOnce(async () => {
    setConnection("http://other.test", "test");
    return new Response(JSON.stringify({ projects: [] }));
  });
  await expect(ensureDefaultProject()).rejects.toThrow("backend changed");
  expect(fetcher).toHaveBeenCalledTimes(2);
});
