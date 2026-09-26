import {afterEach,beforeEach,expect,it,vi} from 'vitest';
const mocks=vi.hoisted(()=>({store:new Map<string,unknown>(),active:true,launch:vi.fn(),follow:vi.fn(),save:vi.fn(),status:vi.fn(),append:vi.fn(),version:1,recover:vi.fn(),failure:vi.fn()}));
vi.mock('../src/api',()=>({connectionVersion:()=>mocks.version,getMission:async()=>({status:mocks.active?'active':'awaiting_user',tags:['placement:client']}),appendClientTranscript:mocks.append,setClientMissionStatus:mocks.status}));
vi.mock('../src/sideQuestionStorage',()=>({sideQuestionKey:()=>`account:${mocks.version}`}));
vi.mock('../src/composerDrafts',()=>({readSideThread:async(k:string)=>structuredClone(mocks.store.get(k)),saveSideThread:async(k:string,v:unknown)=>{mocks.save();mocks.store.set(k,structuredClone(v));}}));
vi.mock('../src/localAgents',()=>({recoverLocalLaunch:mocks.recover,recordLocalFailure:mocks.failure,restoreLocalBindings:async()=>{},localBinding:()=>({cwd:'/work',sessionId:'latest'}),pollLocal:async()=>({done:!mocks.active}),reconcileLocalRun:async()=>{},startLocal:mocks.launch,followLocal:mocks.follow,stopLocal:async()=>{mocks.active=false;}}));
import {enqueueLocalMessage,queuedLocalMessages,startLocalQueueWorker,removeQueuedMessage,takeQueuedMessage,sendQueuedNow,retryQueuedMessage} from '../src/localMessageQueue';
const request={id:'mission',harness:'claudecode',bin:'claude',cwd:'/work',prompt:'first'};
let stop:(()=>void)|undefined;
beforeEach(()=>{vi.useFakeTimers();mocks.store.clear();mocks.recover.mockReset().mockResolvedValue(undefined);mocks.failure.mockReset();mocks.version=1;mocks.active=true;mocks.launch.mockReset().mockResolvedValue({run_id:'r',generation:1});mocks.follow.mockReset().mockResolvedValue({done:true,text:'Done',exit_code:0});mocks.save.mockReset();mocks.status.mockReset();mocks.append.mockReset().mockResolvedValue(undefined);Object.defineProperty(navigator,'locks',{configurable:true,value:{request:async(_key:string,options:unknown,fn?: (lock:unknown)=>unknown)=>fn?fn({name:_key}):(options as ()=>unknown)()}});});
afterEach(()=>{stop?.();vi.useRealTimers();});
it('persists active-run followups and drains them in order using the latest session',async()=>{
 await enqueueLocalMessage(request,'first');await enqueueLocalMessage({...request,prompt:'second'},'second');
 stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(1000);expect(mocks.launch).not.toHaveBeenCalled();
 mocks.active=false;await vi.advanceTimersByTimeAsync(2500);
 expect(mocks.launch.mock.calls.map(c=>c[0].prompt)).toEqual(['first','second']);expect(mocks.launch.mock.calls[0][0].sessionId).toBe('latest');expect(queuedLocalMessages('mission')).toEqual([]);
});
it('restores the queue after remount, permits removal, and Send now stops before sending',async()=>{
 await enqueueLocalMessage(request,'first');await enqueueLocalMessage({...request,prompt:'second'},'second');
 stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(100);stop();
 stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(100);expect(queuedLocalMessages('mission')).toHaveLength(2);
 await removeQueuedMessage(queuedLocalMessages('mission')[1].id);await sendQueuedNow('mission');await vi.advanceTimersByTimeAsync(1100);
 expect(mocks.status).toHaveBeenCalledWith('mission','interrupted');expect(mocks.launch).toHaveBeenCalledTimes(1);
});
it('does not silently replay an uncertain launch or let later messages overtake it',async()=>{
 mocks.active=false;mocks.launch.mockRejectedValue(new Error('connection lost'));
 await enqueueLocalMessage(request,'first');await enqueueLocalMessage({...request,prompt:'second'},'second');stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(4000);
 expect(mocks.launch).toHaveBeenCalledTimes(1);expect(queuedLocalMessages('mission')[0].state).toBe('dispatching');expect(queuedLocalMessages('mission')[0].error).toMatch(/uncertain/);
});
it('keeps a draft unaccepted when durable storage fails',async()=>{
 mocks.save.mockImplementation(()=>{throw Error('disk full');});await expect(enqueueLocalMessage(request,'first')).rejects.toThrow('disk full');expect(mocks.launch).not.toHaveBeenCalled();
});
it('stops dispatching when the connection changes',async()=>{
 await enqueueLocalMessage(request,'first');stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(100);mocks.version=2;mocks.active=false;await vi.advanceTimersByTimeAsync(2000);expect(mocks.launch).not.toHaveBeenCalled();
});

it('wakes immediately for an idle mission without waiting for the polling interval',async()=>{
 mocks.active=false;stop=startLocalQueueWorker();await vi.advanceTimersByTimeAsync(1);
 await enqueueLocalMessage(request,'immediate',{waiting:false});await vi.advanceTimersByTimeAsync(1);
 expect(mocks.launch).toHaveBeenCalledTimes(1);
});
it('does not hold the queue lock while a launch is slow, and cannot cancel a claimed message',async()=>{
 mocks.active=false;let launch!:(value:unknown)=>void;
 mocks.launch.mockImplementation(()=>new Promise(resolve=>launch=resolve));
 stop=startLocalQueueWorker();await enqueueLocalMessage(request,'first');await vi.advanceTimersByTimeAsync(1);
 await enqueueLocalMessage({...request,prompt:'second'},'second');
 const rows=queuedLocalMessages('mission');expect(rows).toHaveLength(2);
 await expect(removeQueuedMessage(rows[0].id)).rejects.toThrow('already been sent');
 await removeQueuedMessage(rows[1].id);expect(queuedLocalMessages('mission')).toHaveLength(1);
 launch({run_id:'r',generation:1});await vi.advanceTimersByTimeAsync(1);
});
it('keeps accepted messages durable across sync failure and never launches them twice',async()=>{
 mocks.active=false;mocks.append.mockRejectedValue(new Error('offline'));
 stop=startLocalQueueWorker();await enqueueLocalMessage(request,'first');await vi.advanceTimersByTimeAsync(50);
 expect(queuedLocalMessages('mission')[0].state).toBe('accepted');
 expect(queuedLocalMessages('mission')[0].result?.text).toBe('Done');
 mocks.append.mockResolvedValue(undefined);await vi.advanceTimersByTimeAsync(1000);
 expect(mocks.launch).toHaveBeenCalledTimes(1);expect(queuedLocalMessages('mission')).toHaveLength(0);
});
it('saves a stopped partial answer before closing the run receipt and releasing the next message',async()=>{
 mocks.active=false;let finish!:(value:unknown)=>void;
 mocks.follow.mockImplementationOnce(()=>new Promise(resolve=>finish=resolve));
 stop=startLocalQueueWorker();await enqueueLocalMessage(request,'first');await vi.advanceTimersByTimeAsync(1);
 await enqueueLocalMessage({...request,prompt:'second'},'second');
 const stopping=sendQueuedNow('mission');await vi.advanceTimersByTimeAsync(1);
 finish({done:true,text:'Partial answer worth keeping',exit_code:143});await stopping;await vi.advanceTimersByTimeAsync(1);
 const partial=mocks.append.mock.calls.findIndex(c=>c[1]==='assistant'&&c[2]==='Partial answer worth keeping');
 expect(partial).toBeGreaterThanOrEqual(0);
 expect(mocks.append.mock.invocationCallOrder[partial]).toBeLessThan(mocks.status.mock.invocationCallOrder[0]);
 expect(mocks.status.mock.calls[0][1]).toBe('interrupted');
 expect(mocks.launch).toHaveBeenCalledTimes(2);
});

it('requires confirmed recovery before retrying an uncertain native launch',async()=>{
 mocks.active=false;mocks.launch.mockRejectedValueOnce(new Error('another Orb window'));
 stop=startLocalQueueWorker();await enqueueLocalMessage(request,'first');await vi.advanceTimersByTimeAsync(10);
 const id=queuedLocalMessages('mission')[0].id;
 mocks.recover.mockRejectedValueOnce(new Error('still running'));
 await expect(retryQueuedMessage(id)).rejects.toThrow('still running');
 expect(mocks.launch).toHaveBeenCalledTimes(1);
 await retryQueuedMessage(id);await vi.advanceTimersByTimeAsync(10);
 expect(mocks.recover).toHaveBeenCalledWith('mission');expect(mocks.launch).toHaveBeenCalledTimes(2);
});
it('retains the real native failure even when the agent produced partial output',async()=>{
 mocks.active=false;mocks.follow.mockResolvedValue({done:true,text:'Partial',exit_code:0,error:'goal objective must be at most 4000 characters'});
 stop=startLocalQueueWorker();await enqueueLocalMessage(request,'first');await vi.advanceTimersByTimeAsync(10);
 expect(mocks.failure).toHaveBeenCalledWith('mission','goal objective must be at most 4000 characters');
 expect(mocks.status).toHaveBeenCalledWith('mission','failed',expect.anything());
});

it('editing removes only a still-unsent message and rejects stale edits',async()=>{
 const id=await enqueueLocalMessage(request,'editable');
 expect(await takeQueuedMessage(id)).toBe('editable');
 expect(queuedLocalMessages('mission')).toHaveLength(0);
 await expect(takeQueuedMessage(id)).rejects.toThrow('already been sent');
 const pending=await enqueueLocalMessage(request,'sending');
 for(const rows of mocks.store.values())if(Array.isArray(rows))for(const row of rows)if(row.id===pending)row.state='dispatching';
 await expect(takeQueuedMessage(pending)).rejects.toThrow('already been sent');
});
