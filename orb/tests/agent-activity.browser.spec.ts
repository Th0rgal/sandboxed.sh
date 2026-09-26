import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('live work, completed tasks and failures remain distinct on desktop and mobile',async({page})=>{
 await page.goto('/tests/agent-activity.html');
 await expect(page.locator('.agent-history-toggle.has-current')).toHaveCount(1);
 await expect(page.locator('.agent-task')).toHaveCount(0);
 await expect(page.getByText('Background work',{exact:true})).toHaveCount(0);
 await expect(page.locator('.agent-history-toggle')).toContainText('Inventory root files, documentation and scripts');
 await expect(page.getByText('Map the Vault importer dependencies')).not.toBeVisible();
 await expect(page.getByText('Validate generated reports')).not.toBeVisible();
 await page.locator('.agent-history-toggle').click();
 await expect(page.getByText('Regenerate verification artifacts')).toBeVisible();
 await expect(page.locator('.history-chevron')).toHaveCSS('transform','matrix(0, -1, 1, 0, 0, 0)');
 await page.getByText('Validate generated reports').click();
 await expect(page.getByText('The trust-surface report is stale.',{exact:false})).toBeVisible();
 await page.screenshot({path:'/tmp/orb-background-work-desktop.png'});
 await page.setViewportSize({width:390,height:844});
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);

 await expect(page.getByText('Map the Vault importer dependencies')).toBeVisible();
 await page.screenshot({path:'/tmp/orb-background-work-mobile.png'});
});

test('successful turns awaiting a follow-up collapse activity and resumed turns reveal it again',async({page})=>{
 await page.goto('/tests/agent-activity.html');
 const activity=page.getByRole('region',{name:'Agent activity'});
 await expect(activity).toBeVisible();
 await page.evaluate(()=>(window as any).completeActivity());
 const collapsed=page.locator('.agent-activity');
 await expect(collapsed).toHaveAttribute('inert','');
 await expect(collapsed).toHaveAttribute('aria-hidden','true');
 await expect(collapsed).toBeHidden();
 await expect.poll(async()=>(await collapsed.boundingBox())?.height ?? 0).toBe(0);
 await page.evaluate(()=>(window as any).resumeActivity());
 await expect(activity).toBeVisible();
 await expect(activity).not.toHaveAttribute('inert','');
 await expect(page.locator('.agent-history-toggle')).toContainText('Inventory root files, documentation and scripts');
 await page.emulateMedia({reducedMotion:'reduce'});
 await page.evaluate(()=>(window as any).completeActivity());
 await expect(collapsed).toHaveCSS('transition-duration','0s');
 await expect(collapsed).toBeHidden();
});

test('history opens upward without moving its toggle and loads earlier rows near the top',async({page})=>{
 await page.setViewportSize({width:800,height:400});
 await page.goto('/tests/agent-activity.html?long');
 const toggle=page.locator('.agent-history-toggle');
 await toggle.scrollIntoViewIfNeeded();
 const before=(await toggle.boundingBox())!.y;
 await toggle.click();
 await expect.poll(async()=>Math.abs((await toggle.boundingBox())!.y-before)).toBeLessThan(2);
 await expect(page.locator('.agent-history-entries .agent-task')).toHaveCount(20);
 const earlier=page.getByRole('button',{name:/Show earlier actions/});
 await earlier.scrollIntoViewIfNeeded();
 await expect.poll(()=>page.locator('.agent-history-entries .agent-task').count()).toBeGreaterThan(20);
});

test('CI waits show one collapsed group with the original actions inside',async({page})=>{
 await page.goto('/tests/agent-activity.html?ci');
 await expect(page.getByText('Waiting for CI',{exact:true})).toBeVisible();
 await expect(page.getByText('Wait for Verity Verify proofs run',{exact:true})).not.toBeVisible();
 await expect(page.getByText('Wait for Verity CI result file',{exact:true})).not.toBeVisible();
 await page.getByText('Waiting for CI',{exact:true}).click();
 await expect(page.getByText('Wait for Verity Verify proofs run',{exact:true})).toBeVisible();
 await expect(page.getByText('Wait for Verity CI result file',{exact:true})).toBeVisible();
});

 test('find searches conversation text and closes with Escape',async({page})=>{
  await page.goto('/tests/agent-activity.html');
  await page.locator('main > p').click();
  await page.keyboard.press('Meta+f');
  const input=page.getByRole('textbox',{name:'Find in conversation'});
  await input.fill('importer');
  await expect(page.locator('.find-count')).toHaveText('1 / 1');
  await input.fill('missing phrase');
  await expect(page.locator('.find-count')).toHaveText('No results');
  await input.press('Escape');
  await expect(input).not.toBeVisible();
 });
