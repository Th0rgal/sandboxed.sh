import { webkit, expect } from '@playwright/test';
const browser = await webkit.launch();
for (const saved of [null, 'a1', 'settings']) {
 const page=await browser.newPage();
 await page.addInitScript(saved=>{ localStorage.clear(); if(saved)localStorage.setItem('orb.selectedConversation',saved); },saved);
 await page.goto('http://127.0.0.1:1432/');
 if(saved==='settings') await page.getByRole('button',{name:'Back',exact:true}).click();
 await expect(page.getByRole('button',{name:/Commit & Push/})).toHaveCount(0);
 await expect(page.getByText(/MSA ~32/)).toHaveCount(0);
 await expect(page.getByRole('button',{name:'Choose project',exact:true})).toBeVisible();
 expect(await page.evaluate(()=>localStorage.getItem('orb.selectedConversation'))).toBe('');
 await page.evaluate(()=>{const d=document.createElement('div');d.className='dock';document.body.append(d);});
 const bg=await page.locator('.dock').last().evaluate(el=>getComputedStyle(el).backgroundColor);
 expect(bg).toBe('rgba(0, 0, 0, 0)');
 await page.close();
}
console.log('Fresh start, stale demo ID, settings-back and transparent dock passed (WebKit).');
await browser.close();
