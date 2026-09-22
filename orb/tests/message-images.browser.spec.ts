import {test,expect} from "@playwright/test";
test.use({browserName:"webkit"});
test("sent images resolve from the local workspace, render numbered thumbnails and open a preview",async({page})=>{
 await page.addInitScript(()=>{
  localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');
  localStorage.setItem('orb.localBindings',JSON.stringify({images:{cwd:'/workspace',harness:'codex',bin:'codex'}}));
  (window as any).__TAURI__={core:{invoke:async (_:string,{request}:any)=>{
   if(request.action==='resolve')return {results:request.paths.map((reference:string)=>({reference,matches:[{name:reference.split('/').pop(),path:reference.replace('/workspace/',''),kind:'file'}]}))};
   if(request.action==='download'){
    const canvas=document.createElement('canvas');canvas.width=240;canvas.height=300;
    const ctx=canvas.getContext('2d')!;ctx.fillStyle='#24343f';ctx.fillRect(0,0,240,300);ctx.fillStyle='#d6e2e8';ctx.font='20px sans-serif';ctx.fillText(request.path.includes('first')?'First image':'Second image',24,48);
    const bytes=Array.from(atob(canvas.toDataURL().split(',')[1]),c=>c.charCodeAt(0));
    return {bytes,size:bytes.length,next:bytes.length};
   }
   return {};
  }}};
 });
 await page.route('**/api/**',r=>r.fulfill({json:r.request().url().includes('file-resources')?{sources:[]}:{job:null,runs:[]}}));
 await page.goto('/tests/message-images.html');
 const first=page.getByRole('button',{name:'Image #1',exact:true});
 await expect(first).toBeEnabled();await expect(page.getByRole('button',{name:'Image #2',exact:true})).toBeEnabled();
 await expect(page.locator('.user')).not.toContainText('Uploaded:');
 await expect(page.locator('.user')).toContainText('Compare #1 and #2');
 await page.screenshot({path:'test-results/orb-sent-images.png'});
 await first.click();await expect(page.getByRole('dialog',{name:'Image #1'})).toBeVisible();
 await page.getByRole('button',{name:'Close',exact:true}).click();await expect(page.getByRole('dialog')).toHaveCount(0);
});
