import {describe,it,expect,vi} from 'vitest';
import {render,screen,fireEvent,waitFor,cleanup} from '@solidjs/testing-library';
import {NativeInteraction} from '../src/NativeInteraction';
import {api} from '../src/api';
vi.mock('../src/api',()=>({api:vi.fn()}));
import {composerModes,modePrompt} from '../src/goal';

describe('native plan interactions',()=>{
 it('only exposes Plan when the destination confirms support',()=>{
  for(const harness of ['codex','claudecode','opencode','grok','gemini','chatgpt']) {
   expect(composerModes(harness).some(m=>m.id==='plan')).toBe(false);
  }
  for (const harness of ['codex', 'claudecode']) {
   expect(composerModes(harness,true).some(m=>m.id==='plan')).toBe(true);
  }
  expect(modePrompt('plan','Build it')).toBe('/plan Build it');
 });
 it('recovers a pending question and sends its native request identity only once',async()=>{
  let pending:any={id:'native-1',method:'questions',params:{questions:[{id:'greeting',question:'Which greeting?',options:[{label:'Hello',description:'English'}]}]}};
  const invoke=vi.fn(async(cmd:string,args:any)=>{
   if(cmd==='local_interaction')return pending;
   expect(args).toEqual({id:'mission',requestId:'native-1',answer:{answers:{greeting:{answers:['Hello']}}}});
   pending=null;return null;
  });
  const host=window as any;const previous=host.__TAURI_INTERNALS__;host.__TAURI_INTERNALS__={invoke};
  try{
   render(()=><NativeInteraction mission="mission" active/>);
   await screen.findByText('Which greeting?');
   fireEvent.click(screen.getByRole('radio'));
   fireEvent.click(screen.getByRole('button',{name:'Continue'}));
   await waitFor(()=>expect(screen.queryByRole('button',{name:'Continue'})).toBeNull());
   expect(invoke.mock.calls.filter(([cmd])=>cmd==='local_interaction_answer')).toHaveLength(1);
  }finally{cleanup();host.__TAURI_INTERNALS__=previous;}
 });
 it('switching from a custom answer to an option clears the custom field',async()=>{
  const invoke=vi.fn(async()=>({id:'custom',method:'questions',params:{questions:[{id:'q',question:'Where?',options:[{label:'Locally'}]}]}}));
  const host=window as any;const previous=host.__TAURI_INTERNALS__;host.__TAURI_INTERNALS__={invoke};
  try{
   render(()=><NativeInteraction mission="mission" active/>);
   const input=await screen.findByRole('textbox',{name:'Other answer: Where?'});
   fireEvent.input(input,{target:{value:'Elsewhere'}});
   expect((input as HTMLInputElement).value).toBe('Elsewhere');
   fireEvent.click(screen.getByRole('radio'));
   expect((input as HTMLInputElement).value).toBe('');
  }finally{cleanup();host.__TAURI_INTERNALS__=previous;}
 });
 it('does not accept a plan until explicitly clicked',async()=>{
  const invoke=vi.fn(async(cmd:string)=>cmd==='local_interaction'?{id:'plan-1',method:'plan',params:{plan:'Create hello.txt'}}:null);
  const host=window as any;const previous=host.__TAURI_INTERNALS__;host.__TAURI_INTERNALS__={invoke};
  try{
   render(()=><NativeInteraction mission="mission" active/>);
   await screen.findByText('Create hello.txt');
   expect(invoke.mock.calls.every(([cmd])=>cmd==='local_interaction')).toBe(true);
   fireEvent.click(screen.getByRole('button',{name:'Implement plan'}));
   await waitFor(()=>expect(invoke).toHaveBeenCalledWith('local_interaction_answer',{id:'mission',requestId:'plan-1',answer:{action:'accept',feedback:''}}));
  }finally{cleanup();host.__TAURI_INTERNALS__=previous;}
 });
 it('retains an expired remote request with an error and sends JSON to the exact tool call',async()=>{
  vi.mocked(api).mockResolvedValue({delivered:false});
  try {
   render(()=><NativeInteraction mission="remote" active remote items={[{kind:'tool',key:'r',callId:'native-remote',name:'ui_native_request',args:{method:'plan',params:{}},done:false}]}/>);
   fireEvent.click(await screen.findByRole('button',{name:'Implement plan'}));
   await screen.findByRole('alert');
   expect(api).toHaveBeenCalledWith('/api/control/tool_result',expect.objectContaining({headers:{'Content-Type':'application/json'},body:JSON.stringify({tool_call_id:'native-remote',name:'ui_native_request',result:{action:'accept',feedback:''}})}));
   expect(screen.getByRole('button',{name:'Implement plan'})).toBeTruthy();
  } finally {cleanup();}
 });
 it('sends requested changes without accepting execution',async()=>{
  const invoke=vi.fn(async(cmd:string)=>cmd==='local_interaction'?{id:'revise',method:'plan',params:{}}:null);
  const host=window as any;const previous=host.__TAURI_INTERNALS__;host.__TAURI_INTERNALS__={invoke};
  try {
   render(()=><NativeInteraction mission="mission" active/>);
   await screen.findByRole('button',{name:'Request changes'});
   fireEvent.input(screen.getByRole('textbox',{name:'Requested changes'}),{target:{value:'Use a single file'}});
   fireEvent.click(screen.getByRole('button',{name:'Request changes'}));
   await waitFor(()=>expect(invoke).toHaveBeenCalledWith('local_interaction_answer',{id:'mission',requestId:'revise',answer:{action:'revise',feedback:'Use a single file'}}));
  } finally {cleanup();host.__TAURI_INTERNALS__=previous;}
 });

});
