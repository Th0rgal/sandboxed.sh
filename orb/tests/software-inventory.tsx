import {render} from 'solid-js/web';
import {AgentSoftware} from '../src/AgentSoftware';
import {ResourceIcon} from '../src/ResourceIcon';
import '../src/styles.css';
localStorage.setItem('orb.apiUrl',location.origin);localStorage.removeItem('orb.jwt');
document.documentElement.dataset.theme=new URLSearchParams(location.search).get('theme')??'dark';
let jobs:any[]=[];
const inventory=()=>({checked_at:Date.now()/1000,runtime:{name:'Orb runner',version:'0.1.0',build:'0.1.0 · build-test',path:'/Applications/Orb.app',restart_required:true},jobs,components:[
 {id:'codex',name:'Codex',version:'codex-cli 1.9.0',path:'/usr/local/bin/codex',installed:true,owner:'npm',update_supported:true,latest:'1.10.0',release_error:null,running:[{session:'session-123',version:'1.8.0',runner:'older-build'}],instructions:'Updates wait for agents to finish.'},
 {id:'opencode',name:'OpenCode',version:'1.0.0',path:'/opt/homebrew/bin/opencode',installed:true,owner:'Homebrew',update_supported:false,latest:'1.1.0',release_error:null,running:[],instructions:'Update with the installer that owns this executable.'},
 {id:'grok',name:'Grok',version:null,path:null,installed:false,owner:'External',update_supported:false,latest:null,release_error:null,running:[],instructions:'Managed externally.'}
]});
(window as any).__TAURI__={core:{invoke:async(name:string,args:any)=>{
 if(name==='software_inventory')return inventory();
 if(name==='software_update'){jobs=[{id:'job-1',component:args.component,version:args.version,path:args.path,state:'queued',error:null}];return jobs[0];}
 if(name==='software_cancel'){jobs=[];return;}
 throw new Error('Unknown command');
}}};
render(()=><div class="page" style={{'max-width':'850px',margin:'30px auto'}}><h2>Machines</h2><div class="machine-resources">{['CPU','Memory','Disk','GPU'].map(kind=><div class="machine-resource"><span><ResourceIcon kind={kind}/>{kind}</span><strong>12%</strong></div>)}</div><AgentSoftware external={[{id:'ssh',name:'SSH machine'}]}/></div>,document.getElementById('root')!);
