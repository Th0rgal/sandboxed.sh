import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test.setTimeout(90000);
test('one pasted image remains visible through delayed send, canonical history and reload on Core',async({page})=>{
 let submitted='',uploadCount=0,downloads=0;
 let releaseUpload!:()=>void,releasePost!:()=>void;
 const uploadGate=new Promise<void>(r=>releaseUpload=r),postGate=new Promise<void>(r=>releasePost=r);
 const path='/srv/context/clipboard.png';
 let png='';
 const mission=()=>({id:'image-mission',project:'test',workspace_id:'workspace',title:'Inspect image',status:'active',backend:'grok',model_override:'grok-4.6',history:submitted?[{role:'user',content:submitted}]:[],created_at:'',updated_at:''});
 await page.addInitScript(()=>{
  localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');
  localStorage.setItem('orb.harnessPick',JSON.stringify({backend:'grok',model:'grok-4.6'}));
  (window as any).rawImageLeaks=[];
  new MutationObserver(()=>{for(const bubble of document.querySelectorAll('.user')) if(/base64,|\[Uploaded:/.test(bubble.textContent??''))(window as any).rawImageLeaks.push(bubble.textContent);}).observe(document,{subtree:true,childList:true,characterData:true});
 });
 await page.route('**/api/**',async route=>{
  const req=route.request(),url=new URL(req.url()),p=url.pathname;
  if(p==='/api/fs/upload'){uploadCount++;await uploadGate;return route.fulfill({json:{path}});}
  if(p==='/api/fs/download'){
   downloads++;
   expect(req.headers().authorization).toBe('Bearer test');
   if(url.searchParams.has('workspace_id'))return route.fulfill({status:404,body:'Outside workspace'});
   return route.fulfill({contentType:'image/png',body:Buffer.from(png,'base64')});
  }
  if(p==='/api/control/missions' && req.method()==='POST'){submitted=req.postDataJSON().prompt;await postGate;return route.fulfill({json:mission()});}
  if(p==='/api/control/missions/image-mission')return route.fulfill({json:mission()});
  if(p.endsWith('/events'))return route.fulfill({json:submitted?[{id:1,event_id:'image-turn',sequence:1,event_type:'user_message',content:submitted,timestamp:''}]:[]});
  if(p==='/api/control/stream')return route.fulfill({contentType:'text/event-stream',body:''});
  const json=p==='/api/projects'?{projects:[{slug:'default',title:'Default'},{slug:'test',title:'Test'}]}:
   p==='/api/backends'?[{id:'grok',name:'Grok'}]:
   p==='/api/providers/backend-models'?{backends:{grok:[{value:'grok-4.6',label:'Grok 4.6'}]}}:
   p==='/api/control/missions'?submitted?[mission()]:[]:
   p==='/api/control/queue'||p==='/api/model-routing/chains'?[]:
   p==='/api/file-resources'?{sources:[]}:
   p==='/api/remote-nodes'?{enabled:true,nodes:[]}:
   p.endsWith('/files')?{entries:[]}:p.endsWith('/crons')?{jobs:[]}:{job:null,runs:[]};
  return route.fulfill({json});
 });
 await page.goto('/',{waitUntil:'domcontentloaded'});
 await expect(page.getByRole('button',{name:'Grok',exact:true})).toBeVisible({timeout:20000});
 const composer=page.locator('.composer textarea').first();
 await composer.fill('Inspect this picture');
 png=await composer.evaluate(el=>{
  const canvas=document.createElement('canvas');canvas.width=40;canvas.height=40;
  const ctx=canvas.getContext('2d')!;ctx.fillStyle='#87a9ca';ctx.fillRect(0,0,40,40);
  const data=canvas.toDataURL().split(',')[1];
  const transfer=new DataTransfer();transfer.items.add(new File([Uint8Array.from(atob(data),c=>c.charCodeAt(0))],'clipboard.png',{type:'image/png'}));
  el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true,cancelable:true}));
  return data;
 });
 await expect(composer).toHaveValue('Inspect this picture[Image #1]');
 await composer.press('Enter');
 const checkImage=async()=>{
  const bubble=page.locator('.user').first();
  await expect(bubble.locator('img')).toHaveCount(1);
  await expect.poll(()=>bubble.locator('img').evaluate((img:HTMLImageElement)=>img.naturalWidth)).toBe(40);
  await expect(bubble).not.toContainText('base64,');await expect(bubble).not.toContainText('[Uploaded:');
  expect((await bubble.textContent())?.match(/\[Image #1\]/g)).toHaveLength(1);
 };
 await checkImage();
 releaseUpload();await expect.poll(()=>submitted).toContain('[Uploaded:');
 await checkImage();releasePost();
 await expect(page.locator('.launch-preview')).toHaveCount(0);
 await checkImage();
 expect(uploadCount).toBe(1);expect(submitted).not.toContain('base64,');
 expect(await page.evaluate(()=>(window as any).rawImageLeaks)).toEqual([]);
 await page.reload({waitUntil:'domcontentloaded'});
 await checkImage();expect(downloads).toBeGreaterThan(0);
 expect(await page.evaluate(()=>(window as any).rawImageLeaks)).toEqual([]);
 await page.screenshot({path:'test-results/orb-image-send-fixed.png'});
});
