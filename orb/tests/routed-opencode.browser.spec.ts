import {test,expect} from '@playwright/test';

test('installed OpenCode is selectable with backend routing profiles only',async({page})=>{
 await page.addInitScript(()=>{
  localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');
  localStorage.setItem('orb.harnessPick',JSON.stringify({backend:'claudecode',model:'opus'}));
 });
 await page.route('**/api/**',route=>{
  const path=new URL(route.request().url()).pathname;
  const json=path==='/api/backends'?[{id:'claudecode',name:'Claude Code'},{id:'opencode',name:'OpenCode'}]:
   path==='/api/providers/backend-models'?{backends:{claudecode:[{value:'opus',label:'Opus'}],opencode:[{value:'openrouter/google/gemini',label:'Gemini'}]}}:
   path==='/api/model-routing/chains'?[{id:'builtin/smart',name:'Smart (Default)'},{id:'reviewer',name:'Reviewer'}]:
   path==='/api/projects'?{projects:[]}:
   path==='/api/control/missions'?[]:
   path==='/api/remote-nodes'?{enabled:true,nodes:[]}:
   path.endsWith('/crons')?{jobs:[]}:{entries:[],job:null,runs:[]};
  return route.fulfill({json});
 });
 await page.goto('/');
 await page.getByRole('button',{name:'Claude Code',exact:true}).click();
 await expect(page.getByRole('button',{name:/OpenCode/})).toBeVisible();
 await page.getByRole('button',{name:/OpenCode/}).click();
 await page.getByRole('button',{name:/Smart \(Default\)/}).click();
 await expect(page.getByRole('button',{name:'Reviewer',exact:true})).toBeVisible();
 await expect(page.getByText('Gemini',{exact:true})).toHaveCount(0);
});
