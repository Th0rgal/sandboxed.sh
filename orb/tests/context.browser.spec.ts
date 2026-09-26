import {test,expect} from '@playwright/test';
test('context conflict comparison fits the panel and keeps resolution conditional',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl','http://context.test');localStorage.setItem('orb.jwt','fixture');});
 const operations:unknown[]=[];
 await page.route('http://context.test/**',async route=>{
  const url=route.request().url();let data:unknown={};
  if(url.endsWith('/manifest'))data={revision:2,entries:{'notes.md':{hash:'shared',directory:false,revision:2,size:12}}};
  if(url.endsWith('/history'))data=[{revision:2,path:'notes.md',entry:{hash:'shared',directory:false,revision:2,size:12},source:'This computer'}];
  if(url.endsWith('/conflicts'))data={variant:{id:'variant',path:'notes.md',base:1,hash:'variant',directory:false,delete:false,source:'DGX Spark'}};
  if(url.includes('/blobs/'))return route.fulfill({body:url.endsWith('/shared')?'# Notes\nShared research notes.':'# Notes\nRevised research notes.',contentType:'text/plain'});
  if(route.request().method()==='POST'){operations.push(route.request().postDataJSON());data={revision:3,conflict:false};}
  await route.fulfill({json:data});
 });
 await page.goto('/tests/context.html');await page.getByRole('button',{name:'History',exact:true}).click();
 await page.getByRole('button',{name:'Compare',exact:true}).click();await expect(page.getByText('Shared version')).toBeVisible();
 const panel=page.getByRole('region',{name:'Context file history'});expect(await panel.evaluate(el=>el.scrollWidth<=el.clientWidth+1)).toBe(true);
 await page.screenshot({path:'/tmp/orb-context-history.png'});
 await page.getByRole('button',{name:'Use variant',exact:true}).click();expect(operations).toEqual([expect.objectContaining({base:2,hash:'variant',path:'notes.md'})]);
 await page.keyboard.press('Escape');await expect(panel).toHaveCount(0);
});
