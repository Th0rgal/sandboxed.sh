import { afterEach, expect, it, vi } from "vitest";
import { sendMissionMessage, createMission, listProjects, api, clearConnection, connectionVersion, getJwt, isConnected, setConnection } from "../src/api";

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
 vi.stubGlobal('fetch',vi.fn(async(input:string)=>new Response(JSON.stringify(input.includes('/model-routing/chains')?[]:input.includes('backend-models')?{backends:{codex:[{value:'model',label:'Model'}]}}:{projects:[{slug:'notes',title:'Notes'}]}))));
 expect((await listProjects())[0].slug).toBe('notes');await listBackendModels();
 vi.stubGlobal('fetch',vi.fn(async()=>{throw new TypeError('offline');}));
 expect((await listProjects())[0].slug).toBe('notes');expect((await listBackendModels()).codex[0].value).toBe('model');
 setConnection('http://offline.test','account-b');await expect(listProjects()).rejects.toThrow('offline');
});

it("replaces a remote explicit-track refusal through create admission with context and identity", async () => {
  setConnection('http://replacement.test','token');
  const source = {id:'source',workspace_id:'00000000-0000-0000-0000-000000000000',remote_job:{node_id:'dgx-spark'},project:'default',track:'mission-original',github_pr:'org/repo#12',tags:['pr-writer'],backend:'claudecode',model_override:'opus',model_effort:'high',history:[{role:'user',content:'Find an address'},{role:'assistant',content:'Result found'}]};
  const fetcher = vi.fn(async (url:string, init?:RequestInit) => {
    if (url.endsWith('/source')) return Response.json(source);
    if (url.endsWith('/message')) return new Response('REMOTE_RESUME_REQUIRES_REPLACEMENT: PR or explicit-track missions need create admission',{status:409});
    return Response.json({id:'replacement',remote_node_id:'dgx-spark',status:'active'});
  });
  vi.stubGlobal('fetch',fetcher);
  const receipt = await sendMissionMessage('source','Read the new public file',[{kind:'file',path:'notes.md'}],'attempt');
  expect(receipt.replacement?.id).toBe('replacement');
  const body=JSON.parse(fetcher.mock.calls[2][1]!.body as string);
  expect(body).toMatchObject({supersedes_mission_id:'source',remote_node_id:'dgx-spark',project:'default',track:'mission-original',github_pr:'org/repo#12',writer:true,backend:'claudecode',model_override:'opus',model_effort:'high',idempotency_key:'orb-followup:source:attempt',attachments:[{kind:'file',path:'notes.md'}]});
  expect(body).not.toHaveProperty('workspace_id');
  expect(body.prompt).toContain('Result found'); expect(body.prompt).toContain('Current user request:\nRead the new public file');
});

it("replays the same replacement create after a lost response without sending the old mission again", async () => {
  setConnection('http://replacement-retry.test','token');
  let creates=0;
  const fetcher=vi.fn(async(url:string,init?:RequestInit)=>{
    if(url.endsWith('/source'))return Response.json({id:'source',remote_node_id:'dgx-spark',history:[]});
    if(url.endsWith('/message'))return new Response('REMOTE_RESUME_REQUIRES_REPLACEMENT: no session',{status:409});
    if(++creates===1)throw new TypeError('network disconnected');
    return Response.json({id:'replacement'});
  });vi.stubGlobal('fetch',fetcher);
  await expect(sendMissionMessage('source','Follow up',undefined,'retry')).rejects.toThrow('network disconnected');
  await expect(sendMissionMessage('source','Follow up',undefined,'retry')).resolves.toMatchObject({replacement:{id:'replacement'}});
  expect(fetcher.mock.calls.filter(([url])=>url.endsWith('/message'))).toHaveLength(1);
  const bodies=fetcher.mock.calls.filter(([url])=>url.endsWith('/missions')).map(([,init])=>init!.body);
  expect(bodies[0]).toEqual(bodies[1]);
});

it.each([['REMOTE_JOB_STILL_RUNNING: wait',409],['REMOTE_RESUME_REQUIRES_REPLACEMENT: unknown outcome',503]])('does not replace for %s',async(detail,status)=>{
  setConnection('http://no-replacement.test','token');
  const fetcher=vi.fn(async(url:string)=>url.endsWith('/source')?Response.json({id:'source',remote_node_id:'dgx-spark'}):new Response(detail as string,{status:status as number}));vi.stubGlobal('fetch',fetcher);
  await expect(sendMissionMessage('source','Follow up')).rejects.toThrow(detail as string);
  expect(fetcher).toHaveBeenCalledTimes(2);
});


it("preserves a dedicated remote worktree so real occupancy protection still applies",async()=>{
  setConnection('http://dedicated.test','token');
  const fetcher=vi.fn(async(url:string,init?:RequestInit)=>{
    if(url.endsWith('/source'))return Response.json({id:'source',remote_node_id:'dgx-spark',workspace_id:'dedicated-workspace'});
    if(url.endsWith('/message'))return new Response('REMOTE_RESUME_REQUIRES_REPLACEMENT: explicit track',{status:409});
    return new Response(JSON.stringify({error:'workspace_occupied',mission_id:'actual-occupant'}),{status:409});
  });vi.stubGlobal('fetch',fetcher);
  await expect(sendMissionMessage('source','Continue',undefined,'dedicated')).rejects.toThrow('workspace_occupied');
  expect(JSON.parse(fetcher.mock.calls[2][1]!.body as string).workspace_id).toBe('dedicated-workspace');
});


it("keeps the last valid project catalog when a successful response is empty or malformed", async () => {
  setConnection("http://projects-cache.test", "test");
  const projects = [{slug:"verity", title:"Verity"}];
  const fetcher = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({projects})))
    .mockResolvedValueOnce(new Response(""))
    .mockResolvedValueOnce(new Response(JSON.stringify({projects:null})));
  vi.stubGlobal("fetch", fetcher);
  await expect(listProjects()).resolves.toEqual(projects);
  await expect(listProjects()).resolves.toEqual(projects);
  await expect(listProjects()).resolves.toEqual(projects);
});

it("rejects an invalid project catalog without caching it and recovers on retry", async () => {
  setConnection("http://projects-retry.test", "test");
  const fetcher = vi.fn().mockResolvedValueOnce(new Response("<html>unavailable</html>"))
    .mockResolvedValueOnce(new Response(JSON.stringify({projects:[{slug:"verity",title:"Verity"}]})));
  vi.stubGlobal("fetch", fetcher);
  await expect(listProjects()).rejects.toThrow("Couldn’t load projects");
  await expect(listProjects()).resolves.toEqual([{slug:"verity",title:"Verity"}]);
});
