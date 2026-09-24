import { afterEach, expect, it, vi } from "vitest";
import { sendMissionMessage, createMission, api, clearConnection, connectionVersion, getJwt, isConnected, setConnection } from "../src/api";

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


it.each(["grok", "codex", "gemini", "chatgpt_ui", "unknown", undefined])("sends remote %s exactly as selected: harness support is server-advertised, not hardcoded here", async backend => {
  // The pre-POST refusal lives in remoteLaunchPreflight (launch.test.ts) and
  // reads GET /api/remote-nodes; createMission itself must never carry a
  // client-side harness list that would reject grok once the server allows it.
  const fetcher = vi.fn(async () => new Response(`REMOTE_HARNESS_UNSUPPORTED: backend '${backend}' cannot run on remote nodes`, {status:400}));
  vi.stubGlobal("fetch", fetcher);
  const body = {remote_node_id:"dgx-spark",backend,model_override:"chosen-model",prompt:"Keep my draft"};
  const original = {...body};
  await expect(createMission(body)).rejects.toThrow("REMOTE_HARNESS_UNSUPPORTED");
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(JSON.parse((fetcher.mock.calls[0] as unknown as [string, RequestInit])[1].body as string)).toEqual(original);
  expect(body).toEqual(original);
});

it.each(["claudecode", "opencode"])("lets the server validate remote %s without changing selection or retrying", async backend => {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => new Response(`Selected ${backend} is unavailable on the node`, {status:400}));
  vi.stubGlobal("fetch", fetcher);
  const body = {remote_node_id:"dgx-spark",backend,model_override:"chosen-model",prompt:"Keep my draft"};
  await expect(createMission(body)).rejects.toThrow(`Selected ${backend} is unavailable`);
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(JSON.parse(fetcher.mock.calls[0][1]!.body as string)).toEqual(body);
});

it.each(["claudecode", "opencode"])("accepts server-provisioned remote %s without shell or credential requests", async backend => {
  const mission = {id:"accepted",status:"active",remote_job:{node_id:"dgx-spark",job_id:"job",phase:"observed"},execution:{state:"waiting_remote_job"}};
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(mission)));
  vi.stubGlobal("fetch", fetcher);
  const body = {remote_node_id:"dgx-spark",backend,model_override:backend === "claudecode" ? "claude-sonnet-4-6" : "xai/grok-4.6",prompt:"Build it",idempotency_key:"attempt"};
  await expect(createMission(body)).resolves.toEqual(mission);
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(fetcher.mock.calls[0][0]).toContain("/api/control/missions");
  expect(JSON.parse(fetcher.mock.calls[0][1]!.body as string)).toEqual(body);
});

it("preserves the selected local harness and model in the supported create contract", async () => {
  const fetcher = vi.fn(async () => new Response(JSON.stringify({id:"accepted"})));
  vi.stubGlobal("fetch", fetcher);
  const body = {backend:"grok",model_override:"grok-4.6",prompt:"Keep my draft"};
  await createMission(body);
  expect(JSON.parse((fetcher.mock.calls[0] as unknown as [string, RequestInit])[1].body as string)).toEqual(body);
});


it("a rejected receipt preserves the follow-up draft and attachments", async () => {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify({ id: "m1", queued: false, message_accepted: false })));
  vi.stubGlobal("fetch", fetcher);
  const attachments = [{kind: "file" as const, path: "notes/test.md"}];
  await expect(sendMissionMessage("mission", "same text", attachments)).rejects.toThrow("not accepted");
  expect(JSON.parse(fetcher.mock.calls[1][1]!.body as string)).toEqual({mission_id: "mission", content: "same text", attachments, client_message_id: expect.any(String)});
  expect(attachments).toHaveLength(1);
});


it("replies with the current writer identity without retagging PR references", async () => {
  const identity = { project: "verity-pareto", track: "mission-47d203db", github_pr: null };
  const fetcher = vi.fn(async (_url: string, init?: RequestInit) => new Response(JSON.stringify(
    init?.method === "POST" ? { id: "reply", queued: true } : { id: "mission", ...identity }
  )));
  vi.stubGlobal("fetch", fetcher);
  await sendMissionMessage("mission", "Et la PR #2441 ?", undefined, "reply");
  expect(JSON.parse(fetcher.mock.calls[1][1]!.body as string)).toEqual({
    mission_id: "mission", content: "Et la PR #2441 ?", client_message_id: "reply", continue_identity: identity,
  });
});

it("keeps remote-node replies on the content-only continuation contract", async () => {
  const fetcher = vi.fn(async (_url: string, init?: RequestInit) => new Response(JSON.stringify(
    init?.method === "POST" ? { id: "reply", queued: true } : { id: "mission", track: "track", remote_node_id: "ashur" }
  )));
  vi.stubGlobal("fetch", fetcher);
  await sendMissionMessage("mission", "Continue", undefined, "reply");
  expect(JSON.parse(fetcher.mock.calls[1][1]!.body as string)).not.toHaveProperty("continue_identity");
});

it("keeps the project/model catalog available offline, without crossing accounts",async()=>{
 const {listProjects,listBackendModels}=await import('../src/api');
 setConnection('http://offline.test','account-a');
 vi.stubGlobal('fetch',vi.fn(async(input:string)=>new Response(JSON.stringify(input.includes('backend-models')?{backends:{codex:[{value:'model',label:'Model'}]}}:{projects:[{slug:'notes',title:'Notes'}]}))));
 expect((await listProjects())[0].slug).toBe('notes');await listBackendModels();
 vi.stubGlobal('fetch',vi.fn(async()=>{throw new TypeError('offline');}));
 expect((await listProjects())[0].slug).toBe('notes');expect((await listBackendModels()).codex[0].value).toBe('model');
 setConnection('http://offline.test','account-b');await expect(listProjects()).rejects.toThrow('offline');
});
