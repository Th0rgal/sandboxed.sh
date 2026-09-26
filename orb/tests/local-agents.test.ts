import { describe, expect, it, vi } from "vitest";
import { isSecretPath, quotePath, rewritePrompt, materializeMentions } from "../src/localAgents";
import type { AttachChip } from "../src/attach";

describe("local mention rewrite", () => {
  it("leaves unknown @words and rewrites a copied file", () => {
    const text = "see @notes/foo.md and email me @home";
    const out = rewritePrompt(text, [{ raw: "@notes/foo.md", absolute: "/tmp/orb/notes/foo.md" }]);
    expect(out).toBe("see /tmp/orb/notes/foo.md and email me @home");
  });

  it("quotes paths that contain spaces", () => {
    expect(quotePath("/tmp/my files/a.md")).toBe('"/tmp/my files/a.md"');
  });

  it("refuses secrets and unreadable files, and copies a readable one", async () => {
    expect(isSecretPath("notes/.env")).toBe(true);
    expect(isSecretPath("notes/foo.md")).toBe(false);
    const chips: AttachChip[] = [
      { id: "f", kind: "file", path: "notes/foo.md", label: "foo" },
      { id: "s", kind: "file", path: ".env", label: "env" },
    ];
    const ok = await materializeMentions(
      "demo",
      "read @notes/foo.md",
      chips,
      async () => "hello",
      async () => [],
    );
    expect(ok.files).toEqual([{ rel: ".paloma/attach/notes/foo.md", content: "hello" }]);
    expect(ok.prompt).toContain("__ROOT__/.paloma/attach/notes/foo.md");
    await expect(materializeMentions("demo", "read @.env", chips, async () => "", async () => [])).rejects.toThrow(/not copied/);
    await expect(
      materializeMentions("demo", "read @notes/foo.md", chips, async () => {
        throw new Error("missing");
      }, async () => []),
    ).rejects.toThrow(/could not be read/);
  });
});

it("restores native session bindings across webview origins", async () => {
  const { restoreLocalBindings, localBinding } = await import('../src/localAgents');
  const binding = {harness:'codex',bin:'/bin/codex',cwd:'/work',sessionId:'original-session'};
  const host = window as unknown as {__TAURI_INTERNALS__?: {invoke: () => Promise<unknown>}};
  const previous = host.__TAURI_INTERNALS__;
  localStorage.removeItem('orb.localBindings');
  host.__TAURI_INTERNALS__ = {invoke: async () => ({mission:binding})};
  try {
    await restoreLocalBindings();
    expect(localBinding('mission')).toEqual(binding);
  } finally {
    host.__TAURI_INTERNALS__ = previous;
    localStorage.removeItem('orb.localBindings');
  }
});

it("restores native bindings even when the webview cache is corrupt", async () => {
  const { restoreLocalBindings, localBinding } = await import('../src/localAgents');
  const binding = {harness:'codex',bin:'/bin/codex',cwd:'/work',sessionId:'original-session'};
  const host = window as unknown as {__TAURI_INTERNALS__?: {invoke: () => Promise<unknown>}};
  const previous = host.__TAURI_INTERNALS__;
  localStorage.setItem('orb.localBindings', '{broken');
  host.__TAURI_INTERNALS__ = {invoke: async () => ({mission:binding})};
  try {
    await restoreLocalBindings();
    expect(localBinding('mission')).toEqual(binding);
  } finally {
    host.__TAURI_INTERNALS__ = previous;
    localStorage.removeItem('orb.localBindings');
  }
});


it("serializes local sends through native recovery and retains the exact receipt", async () => {
  const { startLocal, reconcileLocalRun } = await import('../src/localAgents');
  const { clientRunReceipt } = await import('../src/clientRuns');
  const host = window as any, previous=host.__TAURI_INTERNALS__;
  let release!: (value: unknown) => void;
  const pending=new Promise(resolve=>release=resolve);
  const invoke=vi.fn((command:string)=>command==='local_run_launch'?pending:Promise.resolve({done:true,text:'',resumed:false}));
  host.__TAURI_INTERNALS__={invoke};
  const request={id:'recovery-send',harness:'codex',bin:'/bin/codex',cwd:'/work',prompt:'/plan test'};
  try {
    const first=startLocal(request);
    await reconcileLocalRun(request.id);
    await expect(startLocal(request)).rejects.toThrow('still running locally');
    const receipt={run_id:'new-generation',generation:6,prompt:'test'};release(receipt);
    expect(await first).toEqual(receipt);
    expect(await clientRunReceipt(request.id)).toEqual(receipt);
    expect(invoke.mock.calls.filter(c=>c[0]==='local_run_launch')).toHaveLength(1);
    await reconcileLocalRun(request.id);
  } finally {host.__TAURI_INTERNALS__=previous;}
});

it("resolves context at the cursor token without consuming punctuation or copying snapshots", async()=>{
 const host=window as unknown as {__TAURI_INTERNALS__?:{invoke:ReturnType<typeof vi.fn>}};
 const previous=host.__TAURI_INTERNALS__;
 const invoke=vi.fn().mockResolvedValue({root:"/local/shared context",state:{}});host.__TAURI_INTERNALS__={invoke};
 try{
  const result=await materializeMentions("demo",'Read @context/notes.md. Then @"context/a b.md" and @context.',[]);
  expect(result.prompt).toBe('Read "/local/shared context/notes.md". Then "/local/shared context/a b.md" and "/local/shared context".');
  expect(result.files).toEqual([]);
  expect(invoke.mock.calls[0][1].request.paths).toEqual(['/notes.md','/a b.md','']);
 }finally{host.__TAURI_INTERNALS__=previous;}
});

it('waits for this window’s recovery before launching instead of racing its native lock', async () => {
  const {recoverLocalLaunch,startLocal} = await import('../src/localAgents');
  const host=window as unknown as {__TAURI_INTERNALS__?:{invoke:(command:string)=>Promise<unknown>}};
  const previous=host.__TAURI_INTERNALS__;
  let release!:()=>void;
  const recovering=new Promise<void>(resolve=>release=resolve);
  const invoke=vi.fn(async(command:string)=>{
    if(command==='local_run_reconcile')return recovering;
    if(command==='local_agents_poll')return {done:true,text:''};
    if(command==='local_run_launch')return {run_id:'test-run',generation:1};
    return {};
  });
  host.__TAURI_INTERNALS__={invoke};
  try {
    const recovery=recoverLocalLaunch('recovery-race');
    const launch=startLocal({id:'recovery-race',harness:'codex',bin:'codex',cwd:'/work',prompt:'/goal test'});
    await Promise.resolve();
    expect(invoke.mock.calls.some(c=>c[0]==='local_run_launch')).toBe(false);
    release();await recovery;await launch;
    expect(invoke.mock.calls.filter(c=>c[0]==='local_run_reconcile')).toHaveLength(1);
    expect(invoke.mock.calls.filter(c=>c[0]==='local_run_launch')).toHaveLength(1);
  } finally {host.__TAURI_INTERNALS__=previous;}
});


it("keeps a discovered CLI available when an older native version probe fails", async () => {
  const {refreshLocalAgents, installedIds} = await import("../src/localAgents");
  const previous = (window as any).__TAURI__;
  (window as any).__TAURI__ = {core:{invoke:vi.fn().mockResolvedValue([
    {id:"opencode",bin:"opencode",path:"/opt/homebrew/bin/opencode",installed:false,version:null},
    {id:"grok",bin:"grok",path:null,installed:false,version:null},
  ])}};
  try {
    await refreshLocalAgents();
    expect(installedIds()).toEqual(["opencode"]);
  } finally { (window as any).__TAURI__ = previous; }
});
