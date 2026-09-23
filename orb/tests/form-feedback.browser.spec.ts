import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('pending send moves out of input and restores on rejection',async({page})=>{
 await page.goto('/tests/form-feedback.html');
 const input=page.getByPlaceholder('Send follow-up');
 await input.fill('Please continue with the shorter instructions.');
 await input.press('Enter');
 await expect(input).toHaveValue('');
 await expect(page.getByLabel('Pending message')).toContainText('Please continue');
 await expect(page.getByRole('status')).toHaveText('Sending…');
 await page.screenshot({path:'test-results/pending-send.png'});
 await page.evaluate(()=>(window as any).finish(false));
 await expect(input).toHaveValue('Please continue with the shorter instructions.');
});
test('controller validation stays visible beside save',async({page})=>{
 await page.goto('/tests/form-feedback.html?cron');
 const instruction=page.getByLabel('Instruction',{exact:true});
 await instruction.fill('Some instruction.\n'.repeat(450));
 await expect(instruction).toHaveCSS('border-top-width','0px');
 await page.getByRole('button',{name:'Save',exact:true}).click();
 const error=page.getByText(/Shorten it before saving/);
 await expect(error).toBeInViewport();
 await page.screenshot({path:'test-results/controller-save-validation.png'});
});

test('goal chip moves to actions for multiline drafts',async({page})=>{
 await page.goto('/tests/form-feedback.html');
 const input=page.locator('.composer textarea');
 await input.fill('/goal Ship it');
 const chip=page.locator('.composer > .mode-chip');
 await expect(chip).toBeVisible();
 await input.fill('First paragraph\n\nSecond paragraph');
 await expect(page.locator('.composer')).toHaveClass(/tall/);
 const field=await input.boundingBox();
 const badge=await chip.boundingBox();
 const plus=await page.locator('.composer .plus').boundingBox();
 expect(field!.x).toBeLessThan(badge!.x);
 expect(badge!.y).toBeGreaterThan(field!.y+field!.height-1);
 expect(Math.abs(badge!.y-plus!.y)).toBeLessThan(4);
 await page.screenshot({path:'test-results/goal-composer.png'});
 await input.fill('Short');
 await expect(page.locator('.composer')).not.toHaveClass(/tall/);
 await expect(input).toHaveValue('Short');
});
