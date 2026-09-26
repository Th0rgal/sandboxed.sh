import { cleanup, fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, expect, it, vi } from 'vitest';
import { createSignal } from 'solid-js';
import { Composer } from '../src/App';
import { SideQuestions, type SideQuestionsHandle } from '../src/SideQuestionPanel';
import { askSide, boundedHistory, sideContext } from '../src/sideQuestionClient';
vi.mock('../src/btwAgent',async()=>{const client=await import('../src/sideQuestionClient');return {askBtwAgent:client.askSide,btwSession:()=>undefined,btwActivities:()=>[],btwItems:()=>[],stopBtw:async()=>{},watchBtw:async()=>{}};});
const storage=vi.hoisted(()=>new Map<string,unknown>());
vi.mock('../src/composerDrafts',()=>({
 readComposerDraft:async(key:string)=>storage.get('draft:'+key),
 saveComposerDraft:async(key:string,value:unknown)=>{storage.set('draft:'+key,structuredClone(value));},
 readSideThread:async(key:string)=>storage.get('thread:'+key),
 saveSideThread:async(key:string,value:unknown)=>{storage.set('thread:'+key,structuredClone(value));},
}));
afterEach(()=>{cleanup();vi.unstubAllGlobals();storage.clear();});
it('routes /btw away from the working agent while it is busy',async()=>{
 const send=vi.fn(),ask=vi.fn(()=>true),stop=vi.fn();
 render(()=><Composer placeholder="Follow-up" busy onSend={send} onStop={stop} onBtw={ask}/>);
 fireEvent.input(screen.getByPlaceholderText('Follow-up'),{target:{value:'/btw What is left?'}});
 expect(screen.getByRole('status',{name:'Side question mode'})).toBeTruthy();
 fireEvent.click(screen.getByTitle('Ask side question'));
 await waitFor(()=>expect(ask).toHaveBeenCalledWith('What is left?',[],[]));expect(send).not.toHaveBeenCalled();expect(stop).not.toHaveBeenCalled();
 expect((screen.getByPlaceholderText('Follow-up') as HTMLTextAreaElement).value).toBe('');
});
it('keeps the question when another side question is pending',async()=>{
 render(()=><Composer placeholder="Follow-up" busy onSend={vi.fn()} onStop={()=>{}} onBtw={()=>false}/>);
 fireEvent.input(screen.getByPlaceholderText('Follow-up'),{target:{value:'/btw Why?'}});
 fireEvent.click(screen.getByTitle('Ask side question'));
 await waitFor(()=>expect((screen.getByPlaceholderText('Ask without interrupting…') as HTMLTextAreaElement).value).toBe('Why?'));
});
it('excludes private thinking, queued messages and unfinished replies from the snapshot',()=>{
 expect(sideContext([{kind:'think',key:'a',text:'secret',done:true},{kind:'user',key:'b',text:'queued',queued:true},{kind:'text',key:'c',text:'unfinished',live:true},{kind:'text',key:'d',text:'Finished build',live:false}])).toBe('Agent: Finished build');
 const text=sideContext([{kind:'text',key:'z',text:'é'.repeat(100000),live:false}]);expect(new TextEncoder().encode(text).length).toBeLessThanOrEqual(80000);
 expect(boundedHistory(Array.from({length:25},()=>({question:'Q',answer:'x'.repeat(10000)}))).length).toBe(6);
});
function response(events:unknown[]) {return new Response(new ReadableStream({start(controller){for(const event of events){const bytes=new TextEncoder().encode(`event: btw\r\ndata: ${JSON.stringify(event)}\r\n\r\n`);for(const byte of bytes)controller.enqueue(new Uint8Array([byte]));}controller.close();}}));}
it('decodes split UTF-8/SSE chunks and requires a terminal receipt',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>response([{type:'delta',text:'réussi'},{type:'done',answer:'réussi'}])));
 const receive=vi.fn();await askSide('mission','Question','snapshot',[],new AbortController().signal,receive);
 expect(receive).toHaveBeenCalledWith({type:'done',answer:'réussi'});
 vi.stubGlobal('fetch',vi.fn(async()=>response([{type:'delta',text:'partial'}])));
 await expect(askSide('mission','Question','snapshot',[],new AbortController().signal,receive)).rejects.toThrow('interrupted');
});
it('keeps the side answer separate and transfers only through an explicit draft action',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>response([{type:'start',model:'Assistant'},{type:'done',answer:'The build passed.'}])));
 const transfer=vi.fn();let handle!:SideQuestionsHandle;
 render(()=><SideQuestions mission="side-test" items={[]} ref={h=>handle=h} onTransfer={transfer}/>);
 handle.ask('Status?');await screen.findByText('The build passed.');
 expect(transfer).not.toHaveBeenCalled();fireEvent.click(screen.getByText('Use in agent draft ↗'));
 expect(transfer).toHaveBeenCalledWith(expect.stringContaining('The build passed.'));
});
it('discards a late response when the selected mission changes',async()=>{
 let finish!:(r:Response)=>void;vi.stubGlobal('fetch',vi.fn(()=>new Promise<Response>(resolve=>finish=resolve)));
 const [mission,setMission]=createSignal('first');let handle!:SideQuestionsHandle;
 render(()=><SideQuestions mission={mission()} items={[]} ref={h=>handle=h} onTransfer={()=>{}}/>);
 await handle.ask('First question');setMission('second');finish(response([{type:'done',answer:'Wrong mission answer'}]));
 await new Promise(resolve=>setTimeout(resolve,20));handle.open();
 expect(screen.queryByText('Wrong mission answer')).toBeNull();
});
it('restores an interrupted answer after remount without sending it again',async()=>{
 const fetch=vi.fn(async()=>new Response(new ReadableStream({start(controller){controller.enqueue(new TextEncoder().encode('event: btw\ndata: {"type":"delta","text":"Partial result"}\n\n'));}})));
 vi.stubGlobal('fetch',fetch);
 let handle!:SideQuestionsHandle;
 const mount=()=>render(()=><SideQuestions mission="interrupted-persistence" items={[]} ref={h=>handle=h} onTransfer={()=>{}}/>);
 mount();await handle.ask('Pending question');await screen.findByText('Partial result');cleanup();
 mount();await screen.findByText(/Side question interrupted/);
 expect(screen.getByText('Partial result')).toBeTruthy();
 expect(fetch).toHaveBeenCalledTimes(1);
 expect(screen.getByRole('button',{name:'Retry'})).toBeTruthy();
});

it('edits a side question and resends only to the side lane',async()=>{
 const fetch=vi.fn(async()=>response([{type:'done',answer:'Side answer'}]));
 vi.stubGlobal('fetch',fetch);
 let handle!:SideQuestionsHandle;
 const transfer=vi.fn();
 render(()=><SideQuestions mission="edit-side" items={[]} ref={h=>handle=h} onTransfer={transfer}/>);
 await handle.ask('Original question');await screen.findByText('Side answer');
 fireEvent.dblClick(screen.getByText('Original question'));
 const editor=screen.getByLabelText('Edit prompt text');
 fireEvent.input(editor,{target:{value:'Revised question'}});
 fireEvent.keyDown(editor,{key:'Enter',ctrlKey:true});
 await waitFor(()=>expect(fetch).toHaveBeenCalledTimes(2));
 const [url,init]=fetch.mock.calls[1] as unknown as [string,RequestInit];
 expect(url).toContain('/edit-side/btw');
 expect(JSON.parse(init.body as string).question).toBe('Revised question');
 expect(transfer).not.toHaveBeenCalled();
});
