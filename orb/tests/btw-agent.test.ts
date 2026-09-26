import {it,expect,vi,afterEach} from 'vitest';
import {askBtwAgent,btwSession,stopBtw,btwTurnEvents} from '../src/btwAgent';
import {startLocal,localBinding} from '../src/localAgents';
vi.mock('../src/localAgents',()=>({localBinding:vi.fn(()=>undefined),restoreLocalBindings:async()=>{},refreshLocalAgents:async()=>[{id:'opencode',installed:true,path:'/bin/opencode'}],rememberBinding:vi.fn(),startLocal:vi.fn(async()=>({run_id:'run',generation:1})),followLocal:vi.fn(async()=>({text:'Read fixture',done:true,exit_code:0})),stopLocal:vi.fn(),localActivities:()=>[],reconcileLocalRun:async()=>{},localLiveText:()=> 'Read fixture'}));
import {api,getMission,sendMissionMessage,cancelMission} from '../src/api';
vi.mock('../src/api',async original=>({...await original<typeof import('../src/api')>(),api:vi.fn(),getMission:vi.fn(),sendMissionMessage:vi.fn(),cancelMission:vi.fn(),appendClientTranscript:vi.fn(),setClientMissionStatus:vi.fn()}));
vi.mock('../src/stream',async original=>({...await original<typeof import('../src/stream')>(),getMissionEvents:vi.fn(async()=>[{event_type:'assistant_message',content:'Actual response',sequence:1,id:1,timestamp:''}])}));
afterEach(()=>{localStorage.clear();vi.clearAllMocks();});
it('creates a distinct side agent and never sends to the parent',async()=>{
 vi.mocked(getMission).mockImplementation(async(id)=>({id,status:id==='parent'?'active':'awaiting_user',history:[],tags:[],title:'Main',created_at:'',updated_at:''}));
 vi.mocked(api).mockResolvedValue({id:'child'});
 const events:any[]=[];
 await askBtwAgent('parent','Inspect the repo','Main context',[],new AbortController().signal,e=>events.push(e));
 expect(api).toHaveBeenCalledWith('/api/control/missions/parent/btw/agent',expect.objectContaining({body:expect.stringContaining('builtin/smart')}));
 expect(btwSession('parent')?.id).toBe('child');expect(events.at(-1).type).toBe('done');expect(sendMissionMessage).not.toHaveBeenCalled();
 const {getMissionEvents}=await import('../src/stream');
 vi.mocked(getMissionEvents).mockResolvedValueOnce([]);
 await askBtwAgent('parent','Follow up','New context',[],new AbortController().signal,()=>{});
 expect(sendMissionMessage).toHaveBeenCalledWith('child',expect.stringContaining('New context'));
 await stopBtw('parent');expect(cancelMission).toHaveBeenCalledWith('child');
});
it('a missing agent endpoint fails without falling back to a normal fork',async()=>{
 vi.mocked(getMission).mockResolvedValue({id:'parent',status:'active',history:[],tags:[],title:null,created_at:'',updated_at:''});
 vi.mocked(api).mockRejectedValue(new Error('404'));
 await expect(askBtwAgent('parent','Q','',[],new AbortController().signal,()=>{})).rejects.toThrow('404');
 expect(api).toHaveBeenCalledTimes(1);expect(btwSession('parent')).toBeUndefined();
});

it('launches locally in the parent folder with a fresh session identity',async()=>{
 vi.mocked(localBinding).mockImplementation(id=>id==='local-parent'?{cwd:'/work/shared',harness:'claudecode',bin:'/bin/claude',sessionId:'never-reuse-parent'}:undefined);
 vi.mocked(getMission).mockImplementation(async(id)=>({id,status:id==='local-parent'?'active':'awaiting_user',history:[],tags:id==='local-parent'?['placement:client']:[],title:null,created_at:'',updated_at:''}));
 vi.mocked(api).mockResolvedValue({id:'local-child'});
 await askBtwAgent('local-parent','Read this folder','context',[],new AbortController().signal,()=>{});
 expect(startLocal).toHaveBeenCalledWith(expect.objectContaining({id:'local-child',cwd:'/work/shared',harness:'opencode',model:'builtin/smart',sessionId:undefined}));
});

it('does not turn an empty successful exit into a fabricated answer',async()=>{
 const {getMissionEvents}=await import('../src/stream');
 vi.mocked(getMissionEvents).mockResolvedValue([]);
 vi.mocked(getMission).mockImplementation(async(id)=>({id,status:id==='empty-parent'?'active':'awaiting_user',history:[],tags:[],title:null,created_at:'',updated_at:''}));
 vi.mocked(api).mockResolvedValue({id:'empty-child'});
 const receive=vi.fn();
 await expect(askBtwAgent('empty-parent','Q','',[],new AbortController().signal,receive)).rejects.toThrow('No response was captured');
 expect(receive.mock.calls.some(([e])=>e.type==='done')).toBe(false);
});

it('never replays the previous answer underneath a new user question',()=>{
 const event=(sequence:number,event_type:string,content:string)=>({id:sequence,sequence,event_type,content,timestamp:''});
 const previous=event(4,'assistant_message','Previous CI answer');
 const next=event(7,'user_message','Explain Proof.lean');
 expect(btwTurnEvents([next,previous],{baseline:0})).toEqual([]);
 const answer=event(8,'assistant_message','Proof.lean is the entry point');
 expect(btwTurnEvents([answer,next,previous],{baseline:0})).toEqual([answer]);
 expect(btwTurnEvents([previous],{baseline:0,afterSequence:6})).toEqual([]);
});
