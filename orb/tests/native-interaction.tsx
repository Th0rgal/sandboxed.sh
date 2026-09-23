import {render} from 'solid-js/web';
import {NativeInteraction} from '../src/NativeInteraction';
import {ModeChip} from '../src/goal';
import '../src/styles.css';
document.documentElement.dataset.theme='dark';
const question={id:'question-1',method:'questions',params:{questions:[{id:'storage',question:'Where should settings be saved?',options:[{label:'Locally',description:'Keep settings on this computer.'},{label:'Account',description:'Synchronize across devices.'}]}]}};
let request=JSON.parse(sessionStorage.getItem('pending')??JSON.stringify(question));
(window as any).__TAURI_INTERNALS__={invoke:async(cmd:string,args:any)=>{
 if(cmd==='local_interaction')return request;
 if(cmd==='local_interaction_answer') {
  if(args.requestId!==request.id)throw new Error('Expired request');
  request=request.method==='questions'?{id:'plan-1',method:'plan',params:{plan:'Store settings locally.\n\n- Add local persistence.\n- Verify reloading keeps settings.'}}:null;
  sessionStorage.setItem('pending',JSON.stringify(request));
 }
}};
render(()=><main style={{padding:'30px','max-width':'820px',margin:'auto'}}><NativeInteraction mission="demo" active/><div class="composer tall"><div class="composer-field"><textarea placeholder="Plan before making changes…"/></div><div class="composer-tools"><ModeChip mode="plan" onClear={()=>{}}/></div></div></main>,document.getElementById('root')!);
