import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
// The fixture loads the full composer and its lazy application modules.
test.setTimeout(90000);
test('queue matches the composer width and accepts and removes messages',async({page})=>{
 await page.goto('/tests/queued-messages.html');
 const queue=page.getByRole('region',{name:'Queued messages'});
 await expect(queue).toBeVisible();await expect(queue).toContainText('2 Queued');
 const input=page.getByPlaceholder('Send follow-up');await input.fill('un troisième message');await input.press('Enter');
 await expect(queue).toContainText('3 Queued');await expect(input).toHaveValue('');
 await page.getByRole('button',{name:'Remove queued message: un troisième message'}).focus();await page.getByRole('button',{name:'Remove queued message: un troisième message'}).click();
 await expect(queue).toContainText('2 Queued');
 const panel=await queue.boundingBox(),composer=await page.locator('.composer').boundingBox();
 expect(panel!.width).toBe(composer!.width);expect(panel!.x).toBe(composer!.x);expect(composer!.y-panel!.y-panel!.height).toBe(8);
 await page.screenshot({path:'/tmp/orb-queued-messages.png'});
 await page.setViewportSize({width:390,height:844});expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
});

test('editing returns a queued message to the input without losing an existing draft',async({page})=>{
 await page.goto('/tests/queued-messages.html');
 const queue=page.getByRole('region',{name:'Queued messages'}),input=page.getByPlaceholder('Send follow-up');
 await input.fill('draft in progress');
 const edit=page.getByRole('button',{name:'Edit queued message: et en voici un autre',exact:true});
 await edit.focus();await edit.click();
 await expect(queue).toContainText('1 Queued');
 await expect(queue).not.toContainText('et en voici un autre');
 await expect(input).toHaveValue('draft in progress\n\net en voici un autre');
 await expect(input).toBeFocused();
 await input.fill('message corrigé');await input.press('Enter');
 await expect(queue).toContainText('2 Queued');await expect(queue).toContainText('message corrigé');
 const stored=await page.evaluate(async()=>{
  const db=await new Promise<IDBDatabase>((resolve,reject)=>{const r=indexedDB.open('orb-composer-drafts',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
  return await new Promise<unknown[]>((resolve,reject)=>{const r=db.transaction('drafts').objectStore('drafts').getAll();r.onsuccess=()=>{resolve(r.result);db.close();};r.onerror=()=>reject(r.error);});
 });
 expect(JSON.stringify(stored)).not.toContain('et en voici un autre');
});
