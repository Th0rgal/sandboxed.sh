import { afterEach, expect, it, vi } from "vitest";
import { localAgentsScanning, refreshLocalAgents } from "../src/localAgents";
afterEach(() => { delete (window as any).__TAURI__; vi.restoreAllMocks(); });
it("deduplicates scans, reuses recent results and keeps inventory while refreshing", async () => {
 let finish!: (value: unknown) => void;
 const invoke = vi.fn(() => new Promise(resolve => { finish = resolve; }));
 (window as any).__TAURI__ = { core: { invoke } };
 const a=refreshLocalAgents(true), b=refreshLocalAgents(false);
 expect(a).toBe(b); expect(localAgentsScanning()).toBe(true);
 const rows=[{id:'codex',bin:'codex',path:'/tmp/codex',installed:true}];
 finish(rows); await a;
 expect(localAgentsScanning()).toBe(false);
 expect(await refreshLocalAgents(false)).toEqual(rows);
 expect(invoke).toHaveBeenCalledTimes(1);
 invoke.mockRejectedValueOnce(new Error('scan unavailable'));
 expect(await refreshLocalAgents(true)).toEqual(rows);
 expect(invoke).toHaveBeenCalledTimes(2);
 expect(localAgentsScanning()).toBe(false);
});
