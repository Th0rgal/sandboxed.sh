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
 it("explains unsupported legacy remote API without falling back",()=>{
  expect(launchError(new ApiError(400,"remote_command is required when remote_node_id is set"))).toContain("backend needs an update");
 });
});
