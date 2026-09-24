import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('side conversation stays separate while the agent works',async({page})=>{
 await page.route('**/api/control/missions/browser-fixture/btw',async route=>{
  expect(route.request().postDataJSON().question).toBe('What is left?');
  await route.fulfill({contentType:'text/event-stream',body:'event: btw\ndata: {"type":"start","model":"Assistant"}\n\nevent: btw\ndata: {"type":"done","answer":"The implementation is pushed. **CI is still running.** No current test failure is recorded."}\n\n'});
 });
 await page.goto('/tests/side-questions.html');
 await page.getByPlaceholder('Send follow-up').fill('/btw What is left?');
 await page.getByTitle('Ask side question',{exact:true}).click();
 await expect(page.getByText('CI is still running.',{exact:false})).toBeVisible();
 await expect(page.getByText('Agent is working')).toBeVisible();
 await page.getByPlaceholder('Send follow-up').fill('Keep this draft');
 await page.getByText('Use in agent draft ↗').click();
 await expect(page.getByPlaceholder('Send follow-up')).toHaveValue(/Keep this draft\n\nAbout this side question/);
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
 await page.getByText('Side questions · 1').click();
 await expect(page.getByText('CI is still running.',{exact:false})).toBeVisible();
});
