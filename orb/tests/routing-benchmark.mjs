import { chromium } from '@playwright/test';
const browser=await chromium.launch();
const results=[];
for(let run=0;run<5;run++) {
 const page=await browser.newPage({viewport:{width:1300,height:900}});
 let requests=0;
 await page.route('https://routing.test/**', async route=>{
  requests++;
  const path=new URL(route.request().url()).pathname;
  const wait=path==='/api/providers'?1500:path==='/api/ai/providers'?500:150;
  const json=path.endsWith('/chains')?[{id:'builtin/smart',name:'Smart',is_default:true,strip_thinking:false,entries:Array.from({length:7},(_,i)=>({provider_id:'provider-'+i,model_id:'model-'+i}))}]:path==='/api/providers'?{providers:[]}:[];
  await new Promise(r=>setTimeout(r,wait));
  await route.fulfill({json}).catch(()=>{});
 });
 await page.goto('http://127.0.0.1:1432/tests/routing-perf.html');
 const start=performance.now();
 await page.getByRole('button',{name:'Refresh',exact:true}).waitFor();
 await page.waitForFunction(()=>!Array.from(document.querySelectorAll('button')).find(b=>b.textContent?.trim()==='Refresh')?.disabled);
 const ready=performance.now()-start;
 const coldRequests=requests;
 await page.getByText('Toggle routing').click();
 const warm=performance.now();
 await page.getByText('Toggle routing').click();
 await page.getByRole('button',{name:/Smart builtin/}).waitFor();
 const warmReady=performance.now()-warm;
 results.push({ready:Math.round(ready),warmReady:Math.round(warmReady),coldRequests});
 await page.close();
}
console.log(JSON.stringify(results));
await browser.close();
