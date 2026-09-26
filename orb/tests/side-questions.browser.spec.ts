import {test,expect} from '@playwright/test';
test.use({browserName:'webkit',viewport:{width:1440,height:900}});
test('side conversation stays separate while the agent works',async({page})=>{
 await mockAgent(page,'The implementation is pushed. **CI is still running.** No current test failure is recorded.');
 await page.goto('/tests/side-questions.html');
 await page.getByPlaceholder('Send follow-up').fill('/btw What is left?');
 await page.getByTitle('Ask side question',{exact:true}).click();
 await expect(page.getByText('CI is still running.',{exact:false})).toBeVisible();
 await expect(page.getByText('Agent is working')).toBeVisible();
 await page.getByPlaceholder('Send follow-up').fill('Keep this draft');
 await page.getByText('Use in agent draft ↗').click();
 await expect(page.getByPlaceholder('Send follow-up')).toHaveValue(/Keep this draft\n\nAbout this side question/);
 await page.getByPlaceholder('Ask a side question…').fill('Keep this side draft');
 await expect(page.getByPlaceholder('Ask a side question…')).toHaveCSS('resize','none');
 if(!await page.locator('.btw-sidebar .btw-panel').isVisible())await page.getByLabel('Move side question to right panel').click();
 await expect(page.locator('.btw-sidebar .btw-panel')).toBeVisible();
 await expect(page.getByPlaceholder('Ask a side question…')).toHaveValue('Keep this side draft');
 await page.getByRole('button',{name:'Files',exact:true}).click();
 await expect(page.locator('.btw-sidebar')).not.toBeVisible();
 await page.getByRole('button',{name:'Side question',exact:true}).click();
 await expect(page.locator('.btw-sidebar .btw-panel')).toBeVisible();
 await expect(page.getByPlaceholder('Ask a side question…')).toHaveValue('Keep this side draft');
 await page.getByText('CI is still running.',{exact:true}).dblclick({position:{x:5,y:8}});
 expect(await page.evaluate(()=>window.getSelection()?.toString().trim().length)).toBeGreaterThan(0);
 await page.locator('.btw-sidebar .user').dblclick();
 await expect(page.getByLabel('Edit prompt text')).toHaveValue('What is left?');
 await page.getByLabel('Edit prompt text').press('Escape');
 await expect(page.getByLabel('Edit prompt text')).toHaveCount(0);
 await expect(page.locator('.btw-sidebar .btw-panel')).toBeVisible();
 const fields=await page.locator('.composer textarea').evaluateAll(els=>els.map(el=>{const css=getComputedStyle(el);return {lineHeight:css.lineHeight,paddingTop:css.paddingTop,paddingBottom:css.paddingBottom};}));
 expect(fields[0]).toEqual(fields[1]);
 const sidebar=page.locator('.btw-sidebar');
 const before=(await sidebar.boundingBox())!;
 const divider=await page.getByRole('separator',{name:'Resize side question panel'}).boundingBox();
 await page.mouse.move(divider!.x+4,divider!.y+100);
 await page.mouse.down(); await page.mouse.move(divider!.x-76,divider!.y+100); await page.mouse.up();
 expect((await sidebar.boundingBox())!.width).toBeGreaterThan(before.width+50);
 const resized=(await sidebar.boundingBox())!.width;
 await page.reload();
 await expect(page.locator('.btw-sidebar .btw-panel')).toBeVisible();
 await expect(page.getByText('CI is still running.',{exact:false})).toBeVisible();
 await expect(page.getByPlaceholder('Ask a side question…')).toHaveValue('Keep this side draft');
 expect(Math.abs((await sidebar.boundingBox())!.width-resized)).toBeLessThan(2);
 await page.screenshot({path:'/tmp/orb-btw-sidebar.png'});
 await expect(page.locator('.btw-sidebar .btw-actions')).not.toBeVisible();
 await page.keyboard.press('Escape');
 await expect(page.locator('.btw-sidebar')).not.toBeVisible();
 await page.getByRole('button',{name:'Side question',exact:true}).click();
 await expect(page.getByPlaceholder('Ask a side question…')).toHaveValue('Keep this side draft');
 await page.screenshot({path:'/tmp/orb-btw-desktop.png'});
 await page.setViewportSize({width:390,height:844});
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 await page.screenshot({path:'/tmp/orb-btw-mobile.png'});
 await page.evaluate(()=>document.documentElement.dataset.theme='light');
 const contrast=await page.locator('.btw-panel').evaluate(el=>({fg:getComputedStyle(el).color,bg:getComputedStyle(el).backgroundColor}));
 expect(contrast.fg).not.toBe(contrast.bg);
 await page.screenshot({path:'/tmp/orb-btw-light.png'});
 await page.getByLabel('Close side questions').click();
 await expect(page.getByLabel('Side questions',{exact:true})).not.toBeVisible();
 await page.getByRole('button',{name:'Side question',exact:true}).click();
 await expect(page.getByText('CI is still running.',{exact:false})).toBeVisible();
});

test('drops images and documents into either composer and sends them only to btw',async({page})=>{
 const requests=await mockAgent(page,'Received attachments.');
 await page.goto('/tests/side-questions.html');
 for(const side of [false,true]){
  if(side)await page.getByRole('button',{name:'Side question',exact:true}).click();
  await page.getByPlaceholder(side?'Ask a side question…':'Send follow-up').fill(side?'Describe these files':'/btw Describe these files');
  const input=page.getByPlaceholder(side?'Ask a side question…':'Ask without interrupting…');
  const composer=input.locator('..').locator('..');
  await composer.evaluate(el=>{
   const transfer=new DataTransfer();
   transfer.items.add(new File(['hello world'],'notes.md',{type:'text/markdown'}));
   const png=Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP9sAAAAASUVORK5CYII='),c=>c.charCodeAt(0));
   transfer.items.add(new File([png],'pixel.png',{type:'image/png'}));
   el.dispatchEvent(new DragEvent('drop',{bubbles:true,cancelable:true,dataTransfer:transfer}));
  });
  await expect(composer.locator('.composer-image')).toHaveCount(1);
  await expect(input).toHaveValue(/notes.md/);
  await composer.locator('.send:not(.stop)').click();
  await expect.poll(()=>requests.length).toBe(side?2:1);
  expect(requests.at(-1)).toContain('Attachment: /uploads/');
  await expect(page.getByText('Received attachments.').first()).toBeVisible();
 }
 await page.keyboard.press('Escape');
 await expect(page.locator('.btw-sidebar')).not.toBeVisible();
 await expect(page.locator('.btw-reopen')).toHaveCount(0);
});

async function mockAgent(page:import('@playwright/test').Page,answer:string){
 const requests:string[]=[];const events:any[]=[];
 const mission=(id:string)=>({id,status:id==='browser-fixture'?'active':'awaiting_user',title:'Fixture',history:[],tags:[],created_at:'',updated_at:''});
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/uploads'){const b=route.request().postDataJSON();return route.fulfill({json:{path:'/uploads/'+b.name,name:b.name,size:11}});}
  if(path.endsWith('/btw/agent')){requests.push(route.request().postDataJSON().side_question);events.push({id:events.length+1,sequence:events.length+1,event_type:'assistant_message',content:answer,timestamp:''});return route.fulfill({json:mission('side-child')});}
  if(path==='/api/control/message'){const b=route.request().postDataJSON();expect(b.mission_id).toBe('side-child');requests.push(b.content);events.push({id:events.length+1,sequence:events.length+1,event_type:'assistant_message',content:answer,timestamp:''});return route.fulfill({json:{id:'msg',queued:false}});}
  if(path.endsWith('/events'))return route.fulfill({json:events});
  if(path.includes('/missions/'))return route.fulfill({json:mission(path.split('/').at(-1)!)});
  return route.fulfill({json:[]});
 });
 return requests;
}

test('editing a long side question preserves wrapping, typography and bubble size',async({page})=>{
 await mockAgent(page,'Answer.');
 await page.goto('/tests/side-questions.html');
 const question='Voici un long message avec des détails et plusieurs lignes pour vérifier la stabilité du champ. '.repeat(28);
 await page.getByPlaceholder('Send follow-up').fill('/btw '+question);
 await page.getByTitle('Ask side question',{exact:true}).click();
 await expect(page.getByText('Answer.',{exact:true})).toBeVisible();
 const bubble=page.locator('.btw-thread .user').last();
 await bubble.scrollIntoViewIfNeeded();
 const before=await bubble.evaluate(el=>{const r=el.getBoundingClientRect(),s=getComputedStyle(el);return {height:r.height,width:r.width,font:s.fontSize,line:s.lineHeight,padding:s.paddingRight};});
 await bubble.getByRole('button',{name:'Edit prompt',exact:true}).click();
 const editor=page.getByRole('textbox',{name:'Edit prompt text'});
 await expect(editor).toHaveValue(question.trim());
 const after=await bubble.evaluate(el=>{const r=el.getBoundingClientRect(),s=getComputedStyle(el.querySelector('textarea')!);return {height:r.height,width:r.width,font:s.fontSize,line:s.lineHeight,padding:getComputedStyle(el).paddingRight};});
 expect(Math.abs(after.height-before.height)).toBeLessThanOrEqual(2);
 expect(after.width).toBe(before.width);expect(after.font).toBe(before.font);expect(after.line).toBe(before.line);expect(after.padding).toBe(before.padding);
 await editor.press('Escape');
 expect(Math.abs((await bubble.boundingBox())!.height-before.height)).toBeLessThanOrEqual(2);
});

test('resizing the side panel keeps the visible conversation paragraph anchored',async({page})=>{
 await page.goto('/tests/side-questions.html');
 await page.getByRole('button',{name:'Side question',exact:true}).click();
 await page.evaluate(()=>{
  const main=document.querySelector('main')!;main.classList.add('scroll');
  Object.assign(main.style,{height:'500px',overflow:'auto',display:'block'});
  const content=document.createElement('div');content.className='agent-turn';
  for(let i=0;i<60;i++){const p=document.createElement('p');p.id='anchor-'+i;p.textContent=('Paragraph '+i+' with enough text to wrap when the conversation width changes. ').repeat(4);content.append(p);}
  main.prepend(content);
 });
 await page.locator('#anchor-20').scrollIntoViewIfNeeded();
 await page.evaluate(()=>{const main=document.querySelector('main')!;main.scrollTop+=document.querySelector('#anchor-20')!.getBoundingClientRect().top-main.getBoundingClientRect().top;});
 const before=await page.locator('#anchor-20').evaluate(el=>el.getBoundingClientRect().top);
 const divider=(await page.getByRole('separator',{name:'Resize side question panel'}).boundingBox())!;
 await page.mouse.move(divider.x+4,divider.y+150);await page.mouse.down();await page.mouse.move(divider.x-100,divider.y+150,{steps:8});await page.mouse.up();
 await expect.poll(async()=>Math.abs(await page.locator('#anchor-20').evaluate(el=>el.getBoundingClientRect().top)-before)).toBeLessThan(2);
 await expect(page.locator('main')).not.toHaveAttribute('data-panel-resizing','true');
});

test('side question shares expand and restore shortcuts with Files',async({page})=>{
 await page.goto('/tests/side-questions.html');
 await page.getByRole('button',{name:'Side question',exact:true}).click();
 await page.keyboard.press('Meta+Shift+f');
 await expect(page.locator('.btw-sidebar')).toHaveClass(/maximized/);
 const panelTop=await page.locator('.btw-sidebar').evaluate(el=>el.getBoundingClientRect().top);
 await expect.poll(()=>page.locator('.btw-sidebar .btw-panel').evaluate(el=>el.getBoundingClientRect().top)).toBe(panelTop);
 await expect(page.locator('main')).not.toBeVisible();
 await expect(page.locator('.btw-sidebar')).toBeVisible();
 await page.keyboard.press('Escape');
 await expect(page.locator('main')).toBeVisible();
 await expect(page.locator('.btw-sidebar')).toBeVisible();
});
