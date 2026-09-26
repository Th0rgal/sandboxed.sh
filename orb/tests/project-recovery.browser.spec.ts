import {test,expect} from '@playwright/test';

test('projects recover from an empty successful response without reloading the app',async({page})=>{
 let ready=false;
 await page.addInitScript(()=>{
  localStorage.setItem('orb.apiUrl',location.origin);
  localStorage.setItem('orb.jwt','project-recovery');
 });
 await page.route('**/api/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/projects')return ready
   ?route.fulfill({json:{projects:[{slug:'verity',title:'Verity'}]}})
   :route.fulfill({status:200,body:''});
  if(path==='/api/control/stream')return route.fulfill({contentType:'text/event-stream',body:''});
  const json=path==='/api/backends'||path==='/api/model-routing/chains'||path==='/api/control/missions'||path.endsWith('/queue')?[]
   :path==='/api/providers/backend-models'?{backends:{}}
   :path==='/api/remote-nodes'?{enabled:true,nodes:[]}
   :path.endsWith('/files')?{entries:[]}
   :path.endsWith('/crons')?{jobs:[]}:{job:null,runs:[]};
  return route.fulfill({json});
 });
 await page.goto('/');
 await expect(page.locator('.sb-scroll')).toContainText('Couldn’t load projects');
 await expect(page.locator('.sb-scroll')).not.toContainText('data.projects');
 ready=true;
 await page.locator('.sb-scroll').getByRole('button',{name:'Retry',exact:true}).click();
 await expect(page.getByRole('button',{name:'Verity',exact:true})).toBeVisible();
 await expect(page.locator('.sb-scroll')).not.toContainText('Couldn’t load projects');
});
