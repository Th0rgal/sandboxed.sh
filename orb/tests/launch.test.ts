import { describe,it,expect } from "vitest";
import { initialPrompt,withInitialPrompt,missionDestination,missionPhase,launchError } from "../src/missionLaunch";
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
  expect(withInitialPrompt(events,mission(),receipt)).toBe(events);
  expect(missionDestination(mission({workspace_name:"host"}),receipt)).toBe("DGX Spark");
 });
 it.each(["pending","resuming","interrupted","failed","completed"])("renders honest %s status without text",status=>{
  const phase=missionPhase(mission({status}),false);expect(phase.label).toBeTruthy();expect(phase.detail).toBeTruthy();expect(phase.moving).toBe(["pending","resuming"].includes(status));
 });
 it("explains an older server that requires a remote command",()=>{
  expect(launchError(new ApiError(400,"remote_command is required when remote_node_id is set"))).toContain("does not support structured remote launches");
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
