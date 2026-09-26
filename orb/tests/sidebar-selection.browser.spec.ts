import {test, expect} from '@playwright/test';
test.use({browserName:'webkit'});
for (const action of ['delete', 'move'] as const) test(`sidebar range/toggle selection and batch ${action}`, async ({page}) => {
  await page.addInitScript(() => {localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
  let missions = Array.from({length:4},(_,i)=>({id:`00000000-0000-0000-0000-00000000000${i}`,title:`Agent ${i}`,status:i === 0 || i === 2 ? 'awaiting_user' : 'failed',project:'one',tags:[],history:[],updated_at:`2026-09-2${5-i}`}));
  const writes: string[]=[];
  await page.route('**/api/**',async route=>{
    const req=route.request(),url=new URL(req.url()),path=url.pathname;
    const id=path.split('/')[4];
    if(req.method()==='DELETE') {
      writes.push(id);
      if(id.endsWith('2')) return route.fulfill({status:409,body:'Agent is busy'});
      missions=missions.filter(m=>m.id!==id);return route.fulfill({json:{ok:true}});
    }
    if(req.method()==='POST' && path.endsWith('/project')) {
      writes.push(id); const body=req.postDataJSON();missions=missions.map(m=>m.id===id?{...m,project:body.project,tags:body.tags}:m);
      return route.fulfill({json:{ok:true}});
    }
    const json=path==='/api/projects'?{projects:[{slug:'one',title:'One'},{slug:'two',title:'Two'}]}
      :path==='/api/control/missions'?missions.filter(m=>!url.searchParams.has('project')||m.project===url.searchParams.get('project'))
      :path.startsWith('/api/control/missions/')?missions.find(m=>m.id===id)??{}
      :path.endsWith('/files')?{entries:[]}:path.endsWith('/crons')?{jobs:[]}:path.endsWith('/controller')?{job:null,runs:[]}:[];
    await route.fulfill({json});
  });
  await page.goto('/');await page.getByRole('button',{name:'One',exact:true}).click();
  const tree=page.getByRole('tree',{name:'Projects',exact:true});
  const row=(i:number)=>tree.getByRole('button',{name:`Agent ${i}`,exact:true});
  await row(0).click();await row(2).click({modifiers:['Shift']});
  await expect(tree.locator('[aria-selected="true"]')).toHaveCount(3);
  await row(1).click({modifiers:['Meta']});
  await expect(tree.locator('[aria-selected="true"]')).toHaveCount(2);
  await row(2).click({button:'right'});
  await expect(tree.locator('[aria-selected="true"]')).toHaveCount(2);
  if(action==='delete') {
    await page.getByRole('menuitem',{name:'Delete 2 agents…'}).click();
    await page.getByRole('dialog').getByRole('button',{name:'Delete',exact:true}).click();
    await expect.poll(()=>writes.length).toBe(2);await expect(row(0)).toHaveCount(0);await expect(row(2)).toBeVisible();
    await expect(page.getByText(/Couldn’t delete 1 agent/)).toBeVisible();
  } else {
    await page.getByRole('menuitem',{name:'Move 2 agents',exact:true}).click();
    await page.getByRole('button',{name:'Two',exact:true}).click({button:'right'});
    await page.getByRole('menuitem',{name:'Move 2 agents here',exact:true}).click();
    await expect.poll(()=>writes.length).toBe(2);
    expect(missions.filter(m=>m.project==='two').map(m=>m.title)).toEqual(['Agent 0','Agent 2']);
  }
});

for (const missingAt of ['GET', 'DELETE', 'cascade'] as const) test(`deletion clears an open mission and history (${missingAt})`, async ({page}) => {
  await page.addInitScript(() => {localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
  const id='00000000-0000-0000-0000-000000000009';
  const child='00000000-0000-0000-0000-000000000008';
  let deleting=false, removed=false;
  const mission={id,title:'Open agent',status:'failed',project:'one',history:[],tags:[],updated_at:'2026-09-25'};
  const parent={...mission,id:child,title:'Parent agent'};
  await page.route('**/api/**',async route=>{
    const req=route.request(),url=new URL(req.url()),path=url.pathname;
    if(path===`/api/control/missions/${id}` && deleting && missingAt==='GET') {
      removed=true;return route.fulfill({status:404,body:`Mission ${id} not found`});
    }
    if(req.method()==='DELETE') {
      removed=true;
      return missingAt==='DELETE'?route.fulfill({status:404,body:`Mission ${id} not found`}):route.fulfill({json:{deleted_ids:[child,id]}});
    }
    const json=path==='/api/projects'?{projects:[{slug:'one',title:'One'}]}
      :path==='/api/control/missions'?(removed?[]:missingAt==='cascade'?[mission,parent]:[mission])
      :path===`/api/control/missions/${id}`?mission:path===`/api/control/missions/${child}`?parent
      :path.endsWith('/files')?{entries:[]}:path.endsWith('/crons')?{jobs:[]}:path.endsWith('/controller')?{job:null,runs:[]}:[];
    await route.fulfill({json});
  });
  await page.goto('/');await page.getByRole('button',{name:'One',exact:true}).click();
  const tree=page.getByRole('tree',{name:'Projects',exact:true});
  const row=tree.getByRole('button',{name:'Open agent',exact:true});await row.click();
  await expect.poll(()=>page.evaluate(()=>localStorage.getItem('orb.selectedConversation'))).toBe(`m:${id}`);
  const target=missingAt==='cascade'?tree.getByRole('button',{name:'Parent agent',exact:true}):row;
  await target.click({button:'right'});await page.getByRole('menuitem',{name:'Delete agent…'}).click();
  deleting=true;
  await page.getByRole('dialog').getByRole('button',{name:'Delete',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>localStorage.getItem('orb.selectedConversation'))).toBe('');
  await expect(row).toHaveCount(0);
  await expect(page.getByText(/Couldn’t delete/)).toHaveCount(0);
  // Every navigation entry pointing at the deleted mission was cleared.
  await page.keyboard.press('Meta+[');
  await expect.poll(()=>page.evaluate(()=>localStorage.getItem('orb.selectedConversation'))).not.toBe(`m:${id}`);
});
