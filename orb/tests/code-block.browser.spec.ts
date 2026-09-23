import {test,expect} from '@playwright/test';
test.use({browserName:'webkit'});
test('worker highlighting and bottom-right copy remain usable with overflowing code',async({page})=>{
 await page.addInitScript(()=>Object.defineProperty(navigator,'clipboard',{value:{writeText:async(text:string)=>{(window as any).copied=text;}}}));
 await page.goto('/tests/code-block.html');
 await expect(page.locator('.token.keyword').first()).toHaveText('def');
 await page.getByRole('button',{name:'Copy code'}).first().click();
 expect(await page.evaluate(()=>(window as any).copied)).toBe('def hello():\n    return "hello"');
 const block=page.locator('.md-code-block').last();
 await block.locator('pre').evaluate(el=>{el.scrollLeft=10000;});
 await block.getByRole('button').click();
 expect(await page.evaluate(()=>(window as any).copied)).toContain('last line');
 await page.screenshot({path:'test-results/code-block.png'});
});
