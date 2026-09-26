import { test, expect } from '@playwright/test';
test.use({browserName:'webkit'});
// Small valid two-page document, no external fonts/assets or private content.
function fixture() {
 const objects = ['<< /Type /Catalog /Pages 2 0 R >>','<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
 '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 5 0 R >>',
 '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R >>',
 ...['1 0 0 rg 30 30 240 340 re f','0 0 1 rg 30 30 240 340 re f'].map(s=>`<< /Length ${s.length} >>\nstream\n${s}\nendstream`)];
 let pdf='%PDF-1.4\n';const offsets=[0];
 objects.forEach((s,i)=>{offsets.push(pdf.length);pdf+=`${i+1} 0 obj\n${s}\nendobj\n`;});
 const xref=pdf.length;pdf+=`xref\n0 7\n0000000000 65535 f \n${offsets.slice(1).map(n=>`${String(n).padStart(10,'0')} 00000 n \n`).join('')}trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF`;
 return [...Buffer.from(pdf)];
}
for(const broken of [false,true]) test(`PDF preview ${broken?'reports damaged documents':'renders pages on demand in WebKit'}`,async({page})=>{
 const bytes=broken?[1,2,3]:fixture(); let downloads=0;
 await page.route('**/api/**',route=>{
  const q=route.request().postDataJSON()??{};
  let json:unknown={};
  if(q.action==='roots')json={sources:[{id:'workspace',label:'Workspace',available:true}]};
  if(q.action==='resolve')json={results:q.paths.map((reference:string)=>({reference,matches:reference.includes('IMPLEMENTATION')?[{name:'sample.pdf',path:'sample.pdf',kind:'file'}]:[]}))};
  if(q.action==='list')json={entries:[{name:'sample.pdf',path:'sample.pdf',kind:'file'}]};
  if(q.action==='read')json={binary:true,size:bytes.length,truncated:false};
  if(q.action==='download'){downloads++;json={bytes,size:bytes.length,next:bytes.length};}
  return route.fulfill({json});
 });
 await page.goto('/tests/file-panel.html');
 await page.getByRole('button',{name:'IMPLEMENTATION-BRIEF.md',exact:true}).click();
 await expect(page.getByRole('button',{name:'View PDF',exact:true})).toBeVisible();expect(downloads).toBe(0);
 await page.getByRole('button',{name:'View PDF',exact:true}).click();
 if(broken)await expect(page.getByRole('alert')).toContainText('Couldn’t display this PDF');
 else {
  await expect(page.locator('.pdf-pages')).toHaveAttribute('aria-busy','false');
  const pixel=()=>page.locator('.pdf-pages canvas').evaluate(c=>Array.from((c as HTMLCanvasElement).getContext('2d')!.getImageData(c.width/2,c.height/2,1,1).data));
  expect(await pixel()).toEqual([255,0,0,255]);
  await page.keyboard.press('End');
  await expect(page.locator('.pdf-pagination')).toContainText('2 / 2');
  await page.keyboard.press('Home');
  await expect(page.locator('.pdf-pagination')).toContainText('1 / 2');
  await page.keyboard.press('PageDown');
  await expect(page.locator('.pdf-pagination')).toContainText('2 / 2');
  await page.keyboard.press('ArrowLeft');
  await expect(page.locator('.pdf-pagination')).toContainText('1 / 2');
  await page.getByRole('textbox',{name:'Draft'}).focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.locator('.pdf-pagination')).toContainText('1 / 2');
  await page.getByRole('button',{name:'Next page'}).click();
  await expect(page.locator('.pdf-pagination')).toContainText('2 / 2');
  await expect.poll(pixel).toEqual([0,0,255,255]);
 }
 await page.getByRole('button',{name:'Back',exact:true}).click();
 await expect(page.getByRole('button',{name:'View PDF',exact:true})).toBeVisible();
 await expect(page.locator('.pdf-viewer')).toHaveCount(0);
});
