import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('live work, completed tasks and failures remain distinct on desktop and mobile',async({page})=>{
 await page.goto('/tests/agent-activity.html');
 await expect(page.getByText('2 active')).toBeVisible();
 await expect(page.getByText('Regenerate verification artifacts')).toBeVisible();
 await expect(page.getByText('Map the Vault importer dependencies')).not.toBeVisible();
 await page.getByText('Validate generated reports').click();
 await expect(page.getByText('The trust-surface report is stale.',{exact:false})).toBeVisible();
 await page.screenshot({path:'/tmp/orb-background-work-desktop.png'});
 await page.setViewportSize({width:390,height:844});
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 await page.getByText('1 completed task').click();
 await expect(page.getByText('Map the Vault importer dependencies')).toBeVisible();
 await page.screenshot({path:'/tmp/orb-background-work-mobile.png'});
});
