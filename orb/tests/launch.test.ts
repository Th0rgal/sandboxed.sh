import { describe,it,expect } from "vitest";
import { initialPrompt,withInitialPrompt,missionDestination,missionPhase,launchError,missionSettingsIdle,dockModelLabel } from "../src/missionLaunch";
import { ApiError,type Mission } from "../src/api";
import { buildTranscript } from "../src/Transcript";
const mission=(extra:Partial<Mission>={}):Mission=>({id:"m",title:null,status:"interrupted",history:[],created_at:"",updated_at:"",...extra});
describe("mission launch projection",()=>{
 it("recovers persisted goal when empty history never recorded the initial user event",()=>{
  const m=mission({goal_mode:true,goal_objective:"Check the guard",terminal_reason:"orphan_no_runner"});
  expect(initialPrompt(m)).toBe("/goal Check the guard");expect(withInitialPrompt([],m)).toMatchObject([{kind:"user",text:"/goal Check the guard"}]);
  expect(missionPhase(m,false)).toMatchObject({label:"Interrupted",moving:false,failed:true});
 });
 it("replaces only the optimistic initial turn, preserving real repeated messages",()=>{
  const receipt={prompt:"Hello",nodeId:"dgx-spark",destination:"DGX Spark"};
  expect(withInitialPrompt([],mission(),receipt)).toHaveLength(1);
  const events=buildTranscript([{type:"user_message",data:{id:"a",content:"Hello"}},{type:"user_message",data:{id:"b",content:"Hello"}}]);
  const reconciled=withInitialPrompt(events,mission(),receipt);
  expect(reconciled).toHaveLength(2);
  expect(reconciled[0].key).toBe(withInitialPrompt([],mission(),receipt)[0].key);
  expect(reconciled[1]).toBe(events[1]);
  expect(missionDestination(mission({workspace_name:"host"}),receipt)).toBe("DGX Spark");
 });
 it.each(["pending","resuming","interrupted","failed","completed"])("renders honest %s status without text",status=>{
  const phase=missionPhase(mission({status}),false);expect(phase.label).toBeTruthy();expect(phase.detail).toBeTruthy();expect(phase.moving).toBe(["pending","resuming"].includes(status));
 });
 it("explains an older server that requires a remote command",()=>{
  expect(launchError(new ApiError(400,"remote_command is required when remote_node_id is set"))).toContain("does not support structured remote launches");
 });
 it("treats parked statuses as settings-idle and live turns as not",()=>{
  expect(missionSettingsIdle("awaiting_user")).toBe(true);
  expect(missionSettingsIdle("interrupted")).toBe(true);
  expect(missionSettingsIdle("active")).toBe(false);
  expect(missionSettingsIdle("pending")).toBe(false);
 });
 it("drops a repeated harness prefix from the dock model label",()=>{
  expect(dockModelLabel("Grok","Grok 4.6")).toBe("4.6");
  expect(dockModelLabel("Claude Code","Fable 5.1")).toBe("Fable 5.1");
 });
});


it.each(["observed", "unobserved", "lease_only", "submit_ambiguous"])("remote %s never infers running from Active or old transcript output", phase => {
 const m=mission({status:"active",remote_job:{job_id:"job",node_id:"dgx-spark",phase},execution:{state:"waiting_remote_job"}});
 expect(missionPhase(m,true).label).not.toBe("Running");
 expect(missionDestination(m)).toBe("DGX Spark");
});
it("uses node evidence for queued/running and preserves terminal failures",()=>{
 const m=mission({status:"active",remote_job:{job_id:"job",node_id:"dgx-spark",phase:"observed",node_state:"queued"}});
 expect(missionPhase(m,true).label).toBe("Queued");
 m.remote_job!.node_state="running";
 expect(missionPhase(m,false).label).toBe("Running");
 m.remote_job!.exit_code=1;
 expect(missionPhase(m,true)).toMatchObject({label:"Remote job stopped",failed:true,moving:false});
 m.status="failed";m.remote_job!.terminal_reason="orphan_no_runner";
 expect(missionPhase(m,true)).toMatchObject({label:"Failed",detail:"The backend could not find an active runner."});
});

import { remoteLaunchPreflight, remoteHarnessSupport, remoteHarnessNeedsProxy, remoteLaunchUnconfirmed, TYPED_LAUNCH_UNSUPPORTED } from "../src/missionLaunch";
import type { RemoteNodesResponse, RemoteLaunchCapability } from "../src/api";
const node={id:"dgx-spark",base_url:"",token_env:"",status:"online",labels:[],version:null,capacity_total:null,capacity_available:null,active_jobs:null,queued_jobs:null,last_seen:null,error:null,cordoned:false};
const fleet=(remote_launch?:RemoteLaunchCapability|null,extra:Partial<RemoteNodesResponse>={}):RemoteNodesResponse=>({enabled:true,nodes:[node],...(remote_launch===undefined?{}:{remote_launch}),...extra});
const typed:RemoteLaunchCapability={typed:true,harnesses:["claudecode","opencode"],raw_command:true,proxy_url_configured:true};
const names=(id:string)=>({claudecode:"Claude Code",opencode:"OpenCode",grok:"Grok"} as Record<string,string>)[id]??id;
describe("remote launch preflight follows the server-advertised capability",()=>{
 it("requires the model proxy for advertised remote Codex",()=>{
  const codex={...typed,harnesses:["codex"],proxy_url_configured:false};
  expect(remoteLaunchPreflight(fleet(codex),"dgx-spark",{backend:"codex",model:"gpt-6-astra"})).toContain("model proxy");
  expect(remoteLaunchPreflight(fleet({...codex,proxy_url_configured:true}),"dgx-spark",{backend:"codex",model:"gpt-6-astra"})).toBeNull();
 });
 it("refuses a harness the server has not confirmed and names what it does run",()=>{
  const refusal=remoteLaunchPreflight(fleet(typed),"dgx-spark",{backend:"grok",model:"grok-4.6"},names);
  expect(refusal).toContain("Remote launch for grok (grok-4.6) is not supported on dgx-spark");
  expect(refusal).toContain("Claude Code, OpenCode");expect(refusal).toContain("no mission was submitted");
  expect(remoteLaunchPreflight(fleet(typed),"dgx-spark",{backend:"claudecode",model:"claude-sonnet-4-6"},names)).toBeNull();
 });
 it("accepts grok only once the server advertises it, with nothing hardcoded",()=>{
  const withGrok={...typed,harnesses:[...typed.harnesses!,"grok"]};
  expect(remoteLaunchPreflight(fleet(withGrok),"dgx-spark",{backend:"grok",model:"grok-4.6"},names)).toBeNull();
  expect(remoteHarnessSupport(withGrok,"grok")).toBe("supported");expect(remoteHarnessSupport(typed,"grok")).toBe("unsupported");
  expect(remoteLaunchPreflight(fleet({...typed,harnesses:[]}),"dgx-spark",{backend:"claudecode",model:"m"},names)).toContain("has not enabled any harness");
 });
 it("treats a missing or untyped capability as an older backend and refuses before POST",()=>{
  expect(remoteLaunchPreflight(fleet(),"dgx-spark",{backend:"claudecode",model:"m"})).toBe(TYPED_LAUNCH_UNSUPPORTED);
  expect(remoteLaunchPreflight(fleet(null),"dgx-spark",{backend:"claudecode",model:"m"})).toBe(TYPED_LAUNCH_UNSUPPORTED);
  expect(remoteLaunchPreflight(fleet({typed:false,harnesses:["claudecode"]}),"dgx-spark",{backend:"claudecode",model:"m"})).toBe(TYPED_LAUNCH_UNSUPPORTED);
  expect(remoteHarnessSupport(undefined,"claudecode")).toBe("unknown");expect(remoteHarnessSupport({typed:true},"claudecode")).toBe("unknown");
 });
 it("proxy_url_configured=false blocks only Claude Code/OpenCode, never native Grok OAuth",()=>{
   const noProxy={...typed,harnesses:["claudecode","opencode","grok"],proxy_url_configured:false};
   const claude=remoteLaunchPreflight(fleet(noProxy),"dgx-spark",{backend:"claudecode",model:"m"});
   expect(claude).toContain("cannot reach this backend's model proxy");
   expect(claude).not.toContain("SANDBOXED_PUBLIC_URL");
   expect(remoteLaunchPreflight(fleet(noProxy),"dgx-spark",{backend:"opencode",model:"m"})).toContain("model proxy");
   expect(remoteLaunchPreflight(fleet(noProxy),"dgx-spark",{backend:"grok",model:"grok-4.6"})).toBeNull();
   expect(remoteHarnessNeedsProxy(noProxy,"grok")).toBe(false);
   expect(remoteHarnessNeedsProxy(noProxy,"claudecode")).toBe(true);
   const listed={...noProxy,requires_proxy_harnesses:["grok"]};
   expect(remoteLaunchPreflight(fleet(listed),"dgx-spark",{backend:"grok",model:"grok-4.6"})).toContain("model proxy");
   expect(remoteLaunchPreflight(fleet(listed),"dgx-spark",{backend:"claudecode",model:"m"})).toBeNull();
  });
  it("refuses when the node is unavailable",()=>{
   expect(remoteLaunchPreflight(fleet(typed,{enabled:false}),"dgx-spark",{backend:"claudecode",model:"m"})).toContain("DGX Spark is unavailable");
  expect(remoteLaunchPreflight(fleet(typed,{nodes:[{...node,status:"offline"}]}),"dgx-spark",{backend:"claudecode",model:"m"})).toContain("unavailable");
  expect(remoteLaunchPreflight(fleet(typed,{nodes:[{...node,cordoned:true}]}),"dgx-spark",{backend:"claudecode",model:"m"})).toContain("unavailable");
  expect(remoteLaunchPreflight(fleet(typed),"other",{backend:"claudecode",model:"m"})).toContain("other is unavailable");
 });
 it("explains a failed capability read instead of guessing",()=>{
  expect(remoteLaunchUnconfirmed("dgx-spark",new Error("503 fleet unavailable"))).toBe("Could not confirm remote launch support on DGX Spark: 503 fleet unavailable. Your draft and selection are kept; no mission was submitted.");
 });
 it("keeps the persisted goal fallback for empty-history missions",()=>{
  const m=mission({goal_mode:true,goal_objective:"Original saved objective",history:[]});
  expect(initialPrompt(m)).toBe("/goal Original saved objective");
 });
});

 it("allows the explicitly selected administration profile while ordinary cordons stay unavailable",()=>{
 const admin={...node,id:"dgx-spark-admin",cordoned:true,labels:["administration","manual-only"]};
 const pick={backend:"claudecode",model:"claude-opus-5-5"};
 expect(remoteLaunchPreflight(fleet(typed,{nodes:[admin]}),admin.id,pick)).toBeNull();
 for(const unavailable of [{...admin,labels:[]},{...admin,status:"offline"},{...node,cordoned:true}]) {
 expect(remoteLaunchPreflight(fleet(typed,{nodes:[unavailable]}),unavailable.id,pick)).toContain("unavailable");
 }
 });
