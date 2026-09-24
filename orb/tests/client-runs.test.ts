import { afterEach, expect, it, vi } from "vitest";
import { beginClientRun } from "../src/clientRuns";
afterEach(() => { vi.restoreAllMocks(); delete (window as any).__TAURI_INTERNALS__; });
it("does not bypass execution permits after a transient failure", async () => {
  (window as any).__TAURI_INTERNALS__ = { invoke: vi.fn().mockResolvedValue("11111111-1111-1111-1111-111111111111") };
  const fetch = vi.spyOn(globalThis, "fetch").mockRejectedValue(new Error("network unavailable"));
  await expect(beginClientRun("new", "hello", "/workspace")).rejects.toThrow("network unavailable");
  expect(fetch).toHaveBeenCalledOnce();
});
it("retains old local launches only when the backend lacks transfer capability", async () => {
  (window as any).__TAURI_INTERNALS__ = { invoke: vi.fn().mockResolvedValue("11111111-1111-1111-1111-111111111111") };
  vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response("Not found", { status: 404 }));
  const result = await beginClientRun("legacy", "hello", "/workspace");
  expect(result.receipt.legacy).toBe(true); expect(result.receipt.prompt).toBe("hello");
});
it("requires updating an old native client when the backend has transfer fences", async () => {
  (window as any).__TAURI_INTERNALS__ = { invoke: vi.fn().mockRejectedValue(new Error("unknown command")) };
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ version: 1 })));
  await expect(beginClientRun("updated", "hello", "/workspace")).rejects.toThrow("Update Orb desktop");
});

it("reports native permission failures without pretending the desktop is outdated", async () => {
  (window as any).__TAURI_INTERNALS__ = { invoke: vi.fn().mockRejectedValue("local_machine_identity not allowed") };
  const fetch = vi.spyOn(globalThis, "fetch");
  await expect(beginClientRun("blocked", "hello", "/workspace")).rejects.toThrow("Could not identify this computer: local_machine_identity not allowed");
  expect(fetch).not.toHaveBeenCalled();
});
