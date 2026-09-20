import { afterEach, expect, it, vi } from "vitest";
import { api, clearConnection, connectionVersion, getJwt, isConnected, setConnection } from "../src/api";

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
