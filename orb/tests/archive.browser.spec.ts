import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('archive is immediate, preserves the project, and rolls back a failed request',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
 let projects=0,release!:()=>void;
 const hold=new Promise<void>(r=>release=r);
 const mission={id:'archive-test',title:'Archive this conversation',status:'awaiting_user',history:[],tags:[]};
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path.endsWith('/archive-test/status')){await hold;return route.fulfill({status:500,body:'Archive failed'});}
  if(path==='/api/projects')projects++;
  const json=path==='/api/projects'?{projects:[{slug:'test',title:'Test project'}]}:path==='/api/control/missions'?[mission]:path==='/api/control/missions/archive-test'?mission:path.endsWith('/files')?{entries:[]}:path.endsWith('/crons')?{jobs:[]}:path.endsWith('/controller')?{job:null,runs:[]}:[];
  await route.fulfill({json});
 });
 await page.goto('/');
 const project=page.getByRole('button',{name:'Test project',exact:true});await project.click();
 const row=page.getByRole('button',{name:/Archive this conversation/});await expect(row).toBeVisible();
 const reads=projects;
 await row.click({button:'right'});await page.getByRole('menuitem',{name:'Archive',exact:true}).click();
 await expect(row).not.toBeVisible();
 await expect(project).toHaveAttribute('aria-expanded','true');expect(projects).toBe(reads);
 release();await expect(row).toBeVisible();expect(projects).toBe(reads);
});

test('one collapsed archive spans projects, while completion stays in place; restore reveals the original folder',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
 const missions=[
  {id:'completed',title:'Completed but not archived',status:'completed',project:'one',tags:[]},
  {id:'failed',title:'Failed but not archived',status:'failed',project:'one',tags:[]},
  {id:'archived',title:'Archived nested conversation',status:'acknowledged',project:'two',tags:['orb-folder:notes/deep']},
 ];
 const writes:any[]=[];
 await page.route('**/api/**',async route=>{
  const request=route.request(),url=new URL(request.url()),path=url.pathname;
  if(request.method()==='POST'){
   writes.push({path,body:request.postDataJSON()});
   if(path.endsWith('/archived/status'))missions[2].status=request.postDataJSON().status;
   return route.fulfill({json:{}});
  }
  const json=path==='/api/projects'?{projects:[{slug:'one',title:'First project'},{slug:'two',title:'Second project'}]}
   :path==='/api/control/missions'?missions.filter(m=>(!url.searchParams.has('project')||m.project===url.searchParams.get('project'))&&(!url.searchParams.has('status')||m.status===url.searchParams.get('status')))
   :path==='/api/control/missions/archived'?missions[2]
   :path.endsWith('/files')?{entries:[]}:path.endsWith('/controller')?{job:null,runs:[]}:path.endsWith('/crons')?{jobs:[]}:[];
  await route.fulfill({json});
 });
 await page.goto('/');
 const archives=page.getByRole('button',{name:'Archived',exact:true});
 await expect(archives).toHaveAttribute('aria-expanded','false');
 await page.getByRole('button',{name:'First project',exact:true}).click();
 const projects=page.getByRole('tree',{name:'Projects',exact:true});
 await expect(projects.getByRole('button',{name:/Completed but not archived/})).toBeVisible();
 await expect(projects.getByRole('button',{name:/Failed but not archived/})).toBeVisible();
 await expect(page.getByRole('button',{name:/History ·/})).toHaveCount(0);
 await archives.click();
 const archiveTree=page.getByRole('tree',{name:'Archived conversations'});
 await expect(archiveTree.getByRole('button',{name:'Second project',exact:true})).toHaveAttribute('aria-expanded','false');
 await expect(archiveTree.getByRole('button',{name:/Archived nested conversation/})).toHaveCount(0);
 await archiveTree.getByRole('button',{name:'Second project',exact:true}).click();
 const row=archiveTree.getByRole('button',{name:/Archived nested conversation/});
 await expect(row).toBeVisible();
 await page.screenshot({path:'/tmp/orb-archive-project-groups.png'});
 await expect(projects.getByRole('button',{name:'Second project',exact:true})).toHaveAttribute('aria-expanded','false');
 await row.click({button:'right'});await page.getByRole('menuitem',{name:'Restore',exact:true}).click();
 await expect(archiveTree.getByRole('button',{name:/Archived nested conversation/})).toHaveCount(0);
 await expect(projects.getByRole('button',{name:/Archived nested conversation/})).toBeVisible();
 await expect(projects.getByRole('button',{name:'Second project',exact:true})).toHaveAttribute('aria-expanded','true');
 await expect(page.getByRole('button',{name:'notes',exact:true})).toHaveAttribute('aria-expanded','true');
 await expect(page.getByRole('button',{name:'deep',exact:true})).toHaveAttribute('aria-expanded','true');
 expect(writes).toEqual([{path:'/api/control/missions/archived/status',body:{status:'paused'}}]);
 expect(missions[2].tags).toEqual(['orb-folder:notes/deep']);
 await page.screenshot({path:'/tmp/orb-shared-archives.png'});
 await page.reload();await expect(archives).toHaveAttribute('aria-expanded','false');
});

test('archives load older pages on demand and a rejected restore leaves the session archived',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
 const missions=Array.from({length:101},(_,i)=>({id:`old-${i}`,title:`Old conversation ${i}`,status:'acknowledged',project:'test',tags:[]}));
 const offsets:number[]=[];
 await page.route('**/api/**',async route=>{
  const req=route.request(),url=new URL(req.url()),path=url.pathname;
  if(path.endsWith('/status'))return route.fulfill({status:500,body:'Restore failed'});
  if(path==='/api/control/missions'&&url.searchParams.get('status')==='acknowledged'){
   const offset=Number(url.searchParams.get('offset'));offsets.push(offset);
   return route.fulfill({json:missions.slice(offset,offset+100)});
  }
  const json=path==='/api/projects'?{projects:[{slug:'test',title:'Test project'}]}:path==='/api/control/missions/old-100'?missions[100]:path==='/api/control/missions'?[]:path.endsWith('/files')?{entries:[]}:path.endsWith('/controller')?{job:null,runs:[]}:path.endsWith('/crons')?{jobs:[]}:[];
  await route.fulfill({json});
 });
 await page.goto('/');expect(offsets).toEqual([]);
 await page.getByRole('button',{name:'Archived',exact:true}).click();
 const archive=page.getByRole('tree',{name:'Archived conversations'});
 await archive.getByRole('button',{name:'Test project',exact:true}).click();
 await expect(archive.getByRole('button')).toHaveCount(101);
 await page.getByRole('button',{name:'Load older conversations'}).click();
 await expect(archive.getByRole('button')).toHaveCount(102);expect(offsets).toEqual([0,100]);
 const row=archive.getByRole('button',{name:/Old conversation 100/});await row.click({button:'right'});
 await page.getByRole('menuitem',{name:'Restore',exact:true}).click();
 await expect(page.getByRole('alert')).toContainText('Restore failed');
 await expect(row).toBeVisible();
 await expect(page.getByRole('tree',{name:'Projects',exact:true}).getByRole('button',{name:'Test project',exact:true})).toHaveAttribute('aria-expanded','false');
});
