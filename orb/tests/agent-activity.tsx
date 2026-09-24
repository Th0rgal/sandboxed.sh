import {render} from 'solid-js/web';
import {AgentActivity} from '../src/AgentActivity';
import '../src/styles.css';
const now=Date.now();
render(()=><main style={{padding:'32px','max-width':'760px',margin:'auto'}}>
<p>The importer analysis is back. The repository inventory and verification are still running.</p>
<AgentActivity running items={[
 {id:'task:agent',label:'Inventory root files, documentation and scripts',kind:'agent',background:true,done:false,failed:false,started_at:now-92000,detail:'Latest tool: Read'},
 {id:'task:build',label:'Regenerate verification artifacts',kind:'command',background:true,done:false,failed:false,started_at:now-132000,detail:'python3 scripts/generate_verification_status.py'},
 {id:'task:finished',label:'Map the Vault importer dependencies',kind:'agent',background:true,done:true,failed:false,status:'completed',started_at:now-120000,finished_at:now-30000,detail:'The smoke tests depend on the legacy importer. Update the build targets before removing it.'},
 {id:'task:failed',label:'Validate generated reports',kind:'command',background:true,done:true,failed:true,status:'failed',detail:'The trust-surface report is stale. Regenerate it before rerunning checks.'},
 {id:'t1',label:'Thinking',kind:'thinking',done:true,failed:false},
 {id:'t2',label:'Read repository instructions',kind:'tool',done:true,failed:false},
]}/></main>,document.getElementById('root')!);
