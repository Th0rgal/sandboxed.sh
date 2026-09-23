import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('native question survives reload and the plan waits for explicit approval',async({page})=>{
 await page.goto('/tests/native-interaction.html');
 await expect(page.getByText('Where should settings be saved?')).toBeVisible();
 await page.reload();
 await page.getByRole('radio',{name:'Locally'}).check();
 await page.getByRole('button',{name:'Send answer'}).click();
 await expect(page.getByRole('button',{name:'Implement plan'})).toBeVisible();
 await expect(page.getByRole('button',{name:'Request changes'})).toBeDisabled();
 await page.setViewportSize({width:390,height:844});
 await expect(page.getByRole('button',{name:'Implement plan'})).toBeInViewport();
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 await page.screenshot({path:'/tmp/orb-native-plan-ui.png'});
 await page.getByRole('button',{name:'Implement plan'}).click();
 await expect(page.getByRole('button',{name:'Implement plan'})).toHaveCount(0);
 await page.reload();
 await expect(page.getByRole('button',{name:'Implement plan'})).toHaveCount(0);
});
