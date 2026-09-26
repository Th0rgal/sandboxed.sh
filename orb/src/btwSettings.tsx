import {createSignal} from 'solid-js';
import {Select} from './Select';
import {sideQuestionKey} from './sideQuestionStorage';
export type BtwConfig={harness:string;model:string};
export function btwConfig():BtwConfig {
 try {return {...{harness:'opencode',model:'builtin/smart'},...JSON.parse(localStorage.getItem(sideQuestionKey('settings:btw'))??'{}')};}catch{return {harness:'opencode',model:'builtin/smart'};}
}
export function BtwSettings(){
 const [config,setConfig]=createSignal(btwConfig());const [message,setMessage]=createSignal('');
 const save=()=>{try{localStorage.setItem(sideQuestionKey('settings:btw'),JSON.stringify(config()));setMessage('Saved. Applies to the next side question.');}catch{setMessage('Could not save settings.');}};
 return <div class="s-body settings-body"><div class="s-inner"><h2>Btw</h2><section class="s-sec"><div class="s-card">
 <div class="s-row"><div class="s-row-text"><div class="s-row-title">Harness</div><div class="s-row-desc">An independent agent in the main agent’s folder.</div></div><Select aria-label="Btw harness" value={config().harness} onChange={e=>setConfig({...config(),harness:e.currentTarget.value})}><option value="opencode">OpenCode</option><option value="claudecode">Claude Code</option><option value="codex">Codex</option><option value="grok">Grok</option></Select></div>
 <div class="s-row"><span class="s-row-title">Model</span><input class="s-input" aria-label="Btw model" value={config().model} onInput={e=>setConfig({...config(),model:e.currentTarget.value})}/></div>
 <div class="s-row"><span class="s-row-desc">Normal harness tools and permissions. Orb adds no tool restrictions or token budget. Files are shared with the main agent.</span></div>
 <div class="s-row"><span role="status">{message()}</span><button class="s-btn" disabled={!config().model.trim()} onClick={save}>Save</button></div>
 </div></section></div></div>;
}
