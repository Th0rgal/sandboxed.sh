import { afterEach, expect, it, vi } from "vitest";
import { createMission, api, clearConnection, connectionVersion, getJwt, isConnected, setConnection } from "../src/api";

afterEach(() => { clearConnection(); vi.unstubAllGlobals(); });

it("concurrent unauthorized responses disconnect once", async () => {
  setConnection("http://old.test", "old-token");
  const version = connectionVersion();
  vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 401 })));
  await Promise.allSettled([api("/first"), api("/second")]);
  expect(isConnected()).toBe(false);
  expect(connectionVersion()).toBe(version + 1);
  clearConnection();
  expect(connectionVersion()).toBe(version + 1);
});

it("a previous connection's delayed 401 cannot log out the new connection", async () => {
  setConnection("http://old.test", "old-token");
  let respond!: (response: Response) => void;
  vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(resolve => { respond = resolve; })));
  const pending = api("/crons");
  setConnection("http://new.test", "new-token");
  const version = connectionVersion();
  respond(new Response(null, { status: 401 }));
  await expect(pending).rejects.toThrow("401");
  expect(isConnected()).toBe(true);
  expect(getJwt()).toBe("new-token");
  expect(connectionVersion()).toBe(version);
});


it.each(["grok", "claudecode", "opencode", "codex", "gemini"])("rejects unprovisioned remote %s before any POST or credential request", async backend => {
  const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
  await expect(createMission({remote_node_id:"dgx-spark",backend,model_override:"chosen-model",prompt:"Keep my draft"})).rejects.toThrow(`${backend} (chosen-model)`);
  expect(fetcher).not.toHaveBeenCalled();
});

it("preserves the selected local harness and model in the supported create contract", async () => {
  const fetcher = vi.fn(async () => new Response(JSON.stringify({id:"accepted"})));
  vi.stubGlobal("fetch", fetcher);
  const body = {backend:"grok",model_override:"grok-4.6",prompt:"Keep my draft"};
  await createMission(body);
  expect(JSON.parse((fetcher.mock.calls[0] as unknown as [string, RequestInit])[1].body as string)).toEqual(body);
});
