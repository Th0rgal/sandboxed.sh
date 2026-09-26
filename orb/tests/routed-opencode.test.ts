import {afterEach, beforeEach, expect, it, vi} from 'vitest';
import {clearConnection, listHarnessChoices, setConnection} from '../src/api';

beforeEach(() => { localStorage.clear(); setConnection('https://routing.test', 'test-token'); });
afterEach(() => { clearConnection(); vi.unstubAllGlobals(); });
const chains = [{id:'builtin/smart',name:'Smart',is_default:true},{id:'reviewer',name:'Reviewer'},{id:'team/custom',name:'Custom'}];
function server(routes:unknown = chains) {
 vi.stubGlobal('fetch', vi.fn(async (url:string) => new Response(JSON.stringify(
  url.endsWith('/api/backends') ? [{id:'opencode',name:'OpenCode'},{id:'codex',name:'Codex'}] :
  url.endsWith('/api/model-routing/chains') ? routes :
  {backends:{opencode:[{value:'openrouter/google/gemini',label:'Gemini'}],codex:[{value:'gpt',label:'GPT'}]}}
 ))));
}
it('keeps OpenCode in the picker independently of provider catalog labels', async () => {
 server();
 const choices = await listHarnessChoices();
 expect(choices.map(c=>c.backend.id)).toEqual(['codex','opencode']);
 expect(choices[1].models.map(m=>m.value)).toEqual(chains.map(c=>c.id));
 expect(choices[0].models).toEqual([{value:'gpt',label:'GPT'}]);
});
it('restores routed choices from the same connection cache when offline', async () => {
 server(); const choices = await listHarnessChoices();
 vi.stubGlobal('fetch',vi.fn().mockRejectedValue(new TypeError('offline')));
 expect(await listHarnessChoices()).toEqual(choices);
 setConnection('https://other.test','other-token');
 await expect(listHarnessChoices()).rejects.toThrow('offline');
});
it('rejects malformed routing responses instead of silently hiding OpenCode', async () => {
 server({}); await expect(listHarnessChoices()).rejects.toThrow('Invalid model routing catalog');
});
it('does not offer direct provider models when no routes are configured', async () => {
 server([]); expect((await listHarnessChoices()).map(c=>c.backend.id)).toEqual(['codex']);
});
