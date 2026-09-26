import {createSignal} from 'solid-js';
import {render} from 'solid-js/web';
import {AgentActivity,activityShouldCollapse} from '../src/AgentActivity';
import '../src/styles.css';
import {FindBar} from '../src/FindBar';
const longHistory=new URLSearchParams(location.search).has("long");
const ciWait=new URLSearchParams(location.search).has("ci");
const now=Date.now();
const [completed,setCompleted]=createSignal(false);
Object.assign(window,{completeActivity:()=>setCompleted(true),resumeActivity:()=>setCompleted(false)});
render(()=><main data-find-conversation style={{padding:'32px','max-width':'760px',margin:'auto'}}>
<p style={{'margin-top':longHistory?'600px':undefined}}>The importer analysis is back. The repository inventory and verification are still running.</p>
<AgentActivity running={!completed()} completed={activityShouldCollapse(completed() ? "awaiting_user" : "active", !completed())} items={[
 {id:'task:agent',label:ciWait?'Wait for Verity Verify proofs run':'Inventory root files, documentation and scripts',kind:'agent',background:true,done:false,failed:false,started_at:now-92000,detail:'Latest tool: Read'},
 {id:'task:build',label:ciWait?'Wait for Verity CI result file':'Regenerate verification artifacts',kind:'command',background:true,done:false,failed:false,started_at:now-132000,detail:'python3 scripts/generate_verification_status.py'},
 {id:'task:finished',label:'Map the Vault importer dependencies',kind:'agent',background:true,done:true,failed:false,status:'completed',started_at:now-120000,finished_at:now-30000,detail:'The smoke tests depend on the legacy importer. Update the build targets before removing it.'},
 {id:'task:failed',label:'Validate generated reports',kind:'command',background:true,done:true,failed:true,status:'failed',detail:'The trust-surface report is stale. Regenerate it before rerunning checks.'},
 {id:'t1',label:'Thinking',kind:'thinking',done:true,failed:false},
 {id:'t2',label:'Read repository instructions',kind:'tool',done:true,failed:false},
 ...(longHistory ? Array.from({length:80},(_,i)=>({id:`old:${i}`,label:`Earlier action ${i}`,done:true,failed:false})) : []),
]}/><FindBar/></main>,document.getElementById('root')!);
