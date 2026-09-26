import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('full-width meters, separate account actions and API key add/edit',async({page})=>{
 await page.addInitScript(()=>{localStorage.setItem('orb.apiUrl',location.origin);localStorage.setItem('orb.jwt','test');});
 const accounts=[{id:'quota',name:'OpenAI (person@example.com)',account_email:'person@example.com',provider_type:'openai',uses_oauth:true,status:{type:'connected'}},{id:'key',name:'Work key',provider_type:'openai',uses_oauth:false,status:{type:'connected'}}];
 const usage={provider_type:'openai',codex_primary_used_percent:100,codex_primary_window_minutes:10080,codex_primary_reset_at:Math.floor(Date.now()/1000)+7200};
 const writes:any[]=[];
 await page.route('**/api/**',route=>{
  const req=route.request(),path=new URL(req.url()).pathname;
  if(['POST','PUT'].includes(req.method())) {writes.push({method:req.method(),body:req.postDataJSON()});return route.fulfill({json:{}});}
  return route.fulfill({json:path==='/api/ai/providers'?accounts:path==='/api/ai/providers/usage'?{entries:{quota:usage}}:path.endsWith('/quota/usage')?usage:path==='/api/projects'?{projects:[]}:[]});
 });
 await page.goto('/');await page.getByRole('button',{name:'Providers',exact:true}).click();
 const account=page.locator('.p-acc-wrap').filter({hasText:'person@example.com'});
 await account.locator('.p-acc-btn').click();
 await expect(account.getByRole('button',{name:'Reconnect',exact:true})).toHaveCount(0);
 const meter=account.getByRole('progressbar');await expect(meter).toHaveAttribute('aria-valuenow','100');
 const body=await account.locator('.p-acc-body').boundingBox(),bar=await meter.boundingBox();
 expect(bar!.width).toBeGreaterThan(body!.width-85);
 await expect(account.locator('.p-chip')).toHaveCount(0);
 await account.getByRole('button',{name:/Actions for/}).click();await expect(page.getByRole('menuitem',{name:'Re-authenticate'})).toBeVisible();await page.keyboard.press('Escape');
 await page.getByRole('button',{name:'Add API key',exact:true}).click();
 const dialog=page.getByRole('dialog');await dialog.locator('input').nth(0).fill('New account');await dialog.locator('input[type=password]').fill('test-only-secret');
 await dialog.getByRole('button',{name:'Save',exact:true}).click();await expect(dialog).toHaveCount(0);
 expect(writes[0]).toEqual({method:'POST',body:{provider_type:'openai',name:'New account',api_key:'test-only-secret'}});
 const key=page.locator('.p-acc-wrap').filter({hasText:'Work key'});await key.getByRole('button',{name:/Actions for/}).click();await page.getByRole('menuitem',{name:'Edit API key'}).click();
 await expect(dialog.locator('input[type=password]')).toHaveValue('');await dialog.locator('input[type=password]').fill('replacement-test-key');await dialog.getByRole('button',{name:'Save',exact:true}).click();await expect(dialog).toHaveCount(0);
 expect(writes[1]).toEqual({method:'PUT',body:{name:'Work key',api_key:'replacement-test-key'}});
 await account.locator('.p-acc-btn').click();
 await expect(account.locator('.p-meter-caption')).toContainText('Weekly');
 await page.screenshot({path:'test-results/provider-layout.png'});
});
