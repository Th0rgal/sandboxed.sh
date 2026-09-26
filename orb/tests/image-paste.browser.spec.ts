import {test,expect} from "@playwright/test";
test.use({ browserName: process.env.ORB_TEST_BROWSER === "webkit" ? "webkit" : "chromium" });
test("pasted image stays in the draft until removed or accepted",async({page})=>{
 await page.addInitScript(() => localStorage.setItem('orb-theme','dark'));
 await page.route("**/api/**",r=>r.fulfill({json:{}}));
 await page.goto('/');
 await page.getByRole('button',{name:/^New Agent/}).click();
 const field=page.locator('.composer textarea');
 await field.fill('Inspect this picture');
 await field.evaluate(el=>{
  const transfer=new DataTransfer();
  const bytes=Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1cAAAAASUVORK5CYII='),c=>c.charCodeAt(0));
  transfer.items.add(new File([bytes],'clipboard.png',{type:'image/png'}));
  el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true,cancelable:true}));
 });
 await expect(page.getByAltText('Image #1', {exact:true})).toBeVisible();
 await expect(field).toHaveValue('Inspect this picture[Image #1]');
 await page.screenshot({path:'test-results/orb-image-paste.png'});
 await page.reload();
 await expect(page.getByAltText('Image #1', {exact:true})).toBeVisible();
 await expect(field).toHaveValue('Inspect this picture[Image #1]');
 await page.getByRole('button',{name:'Send',exact:true}).click();
 await expect(page.getByAltText('Image #1', {exact:true})).toBeVisible();
 await expect(field).toHaveValue('Inspect this picture[Image #1]');
 await page.getByRole('button',{name:'Remove image',exact:true}).click();
 await expect(page.getByAltText('Image #1', {exact:true})).toHaveCount(0);
 await expect(field).toHaveValue('Inspect this picture');
});
