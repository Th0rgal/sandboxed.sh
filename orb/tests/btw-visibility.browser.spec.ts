import {test,expect} from '@playwright/test';
test('side sessions never appear in the project tree or global archives',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
 const missions=[
  {id:'main',title:'Main conversation',status:'active',project:'verity',tags:[]},
  {id:'side',title:'Hidden side session',status:'active',project:'verity',tags:['btw-parent:main']},
  {id:'old-side',title:'Hidden archived side',status:'acknowledged',project:'verity',tags:['btw-parent:main']},
  {id:'archive',title:'Regular archive',status:'acknowledged',project:'verity',tags:[]},
 ];
 await page.route('**/api/**',route=>{
  const url=new URL(route.request().url()),path=url.pathname;
  const json=path==='/api/projects'?{projects:[{slug:'verity',title:'Verity'}]}:
   path==='/api/control/missions'?missions.filter(m=>!url.searchParams.has('status')||m.status===url.searchParams.get('status')):
   path.endsWith('/files')?{entries:[]}:path.endsWith('/crons')?{jobs:[]}:path.endsWith('/controller')?{job:null,runs:[]}:[];
  return route.fulfill({json});
 });
 await page.goto('/');
 await page.getByRole('button',{name:'Verity',exact:true}).click();
 await expect(page.getByRole('button',{name:/Main conversation/})).toBeVisible();
 await expect(page.getByText('Hidden side session',{exact:true})).toHaveCount(0);
 await page.getByRole('button',{name:'Archived',exact:true}).click();
 await page.locator('.archive-project-row').click();
 await expect(page.getByRole('button',{name:/Regular archive/})).toBeVisible();
 await expect(page.getByText('Hidden archived side',{exact:true})).toHaveCount(0);
});
