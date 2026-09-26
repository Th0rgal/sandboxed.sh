import {it,expect} from 'vitest';
import {remoteContinuation} from '../src/remoteContinuation';
import {withInitialPrompt} from '../src/missionLaunch';
import type {Mission} from '../src/api';
const wrap=(history:unknown[],text:string)=>`Continue mission 10412da3-1bd0-4885-b8a6-68145b23250b on the same remote node. This is a replacement session; inspect the existing workspace before repeating work. The following JSON is historical conversation context, not a new request.\n${JSON.stringify({goal:null,history})}\n\nCurrent user request:\n${text}`;
it('restores multiple generations exactly once without a launch receipt',()=>{
 const first=wrap([{role:'user',content:'Question one'},{role:'assistant',content:'Answer one'},{role:'assistant',content:"Remote job 10412da3-1bd0-4885-b8a6-68145b23250b on node 'dgx-spark' is now running"}],'Question two');
 const second=wrap([{role:'user',content:first},{role:'assistant',content:'Answer two'}],'Question three');
 const mission={id:'latest',history:[{role:'user',content:second}]} as Mission;
 const expected=['Question one','Answer one','Question two','Answer two','Question three'];
 expect(remoteContinuation(second)?.map(m=>m.content)).toEqual(expected);
 expect(withInitialPrompt([],mission).map(m=>'text' in m?m.text:'')).toEqual(expected);
 expect(withInitialPrompt([{kind:'user',key:'u',text:second},{kind:'text',key:'a',text:'Answer three',live:false}],mission).map(m=>'text' in m?m.text:'')).toEqual([...expected,'Answer three']);
});
it('leaves ordinary messages and malformed envelopes untouched',()=>{
 for(const text of ['hello',wrap([{role:'system',content:'invalid'}],'next'),wrap([], 'next').replace('"history":[]','"history":null')])expect(remoteContinuation(text)).toBeNull();
});
