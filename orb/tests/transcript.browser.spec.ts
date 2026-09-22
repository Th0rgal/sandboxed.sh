import { test, expect } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("long transcript DOM benchmark retains expanded work",async({page})=>{
 await page.goto(`/tests/transcript.html${process.env.ORB_BENCHMARK_BASELINE ? "?baseline" : ""}`);
 await expect(page.locator(".st-work")).toHaveCount(300);
 const result=await page.evaluate(()=> (window as any).transcriptHarness.benchmark());
 console.log("TRANSCRIPT_BENCHMARK",JSON.stringify(result));
 writeFileSync(`test-results/transcript-${process.env.ORB_BENCHMARK_BASELINE ? "before" : "after"}.json`,JSON.stringify(result,null,2));
 if(!process.env.ORB_BENCHMARK_BASELINE){expect(result.foldRetained).toBe(true);expect(result.foldOpen).toBe(true);expect(result.removed).toBeLessThan(500);}
});

test("cumulative live text across tools renders once and preserves open tool details",async({page})=>{
 await page.goto("/tests/transcript.html");
 await page.waitForFunction(()=>!!(window as any).transcriptHarness);
 await page.evaluate(()=> (window as any).transcriptHarness.reset([
  {type:"text_op",data:{bubble_id:"text_delta_latest",ops:[{type:"insert",pos:0,text:"Inspect file."}]}},
  {type:"tool_call",data:{tool_call_id:"a",name:"read",args:{path:"guard.ts"}}},
  {type:"tool_result",data:{tool_call_id:"a",result:"Complete"}}
 ]));
 await page.locator(".st-work-head").click();await page.locator(".st-tool-head").click();
 await page.evaluate(()=> (window as any).transcriptHarness.apply({type:"text_op",data:{bubble_id:"text_delta_latest",ops:[{type:"replace",range:[0,13],text:"Inspect file. Fix guard."}]}}));
 await expect(page.locator(".st-text")).toHaveCount(1);
 await expect(page.locator(".st-text")).toHaveText("Inspect file. Fix guard.");
 await expect(page.locator(".st-caret")).toHaveCount(0);
 await expect(page.locator(".st-work-body")).toBeVisible();await expect(page.locator(".st-tool-detail")).toBeVisible();
 await page.evaluate(()=> (window as any).transcriptHarness.apply({type:"assistant_message",data:{id:"final",content:"Fixed the guard."}}));
 await expect(page.locator(".st-text")).toHaveCount(1);await expect(page.locator(".st-caret")).toHaveCount(0);
 await expect(page.locator(".st-tool-detail")).toBeVisible();
 await page.evaluate(()=> (window as any).transcriptHarness.reset([
  {type:"tool_call",data:{tool_call_id:"a",name:"read",args:{path:"guard.ts"}}},
  {type:"tool_result",data:{tool_call_id:"a",result:"Complete"}},
  {type:"assistant_message",data:{id:"final",content:"Fixed the guard."}}
 ]));
 await expect(page.locator(".st-text")).toHaveCount(1);
 await expect(page.locator(".st-work-body")).toBeVisible();await expect(page.locator(".st-tool-detail")).toBeVisible();
  await page.screenshot({path:"test-results/orb-stream-reconciled.png"});
});

test("native Grok canary final assistant_message then text_delta renders once",async({page})=>{
  const events=(await import("./fixtures/native-canary-events.json",{with:{type:"json"}})).default;
  await page.addInitScript(()=>{localStorage.setItem("orb.apiUrl",location.origin);localStorage.setItem("orb.jwt","test");localStorage.setItem("orb-theme","dark");});
  const mission={id:"44584615-b118-45c4-a4e7-299c2ab1a153",title:"Orb native Grok launch verification",status:"completed",history:[],workspace_name:"host",remote_node_id:"dgx-spark",created_at:"",updated_at:""};
  await page.route("**/api/**",async route=>{
    const path=new URL(route.request().url()).pathname;
    const json=path==="/api/projects"?{projects:[{slug:"test",title:"test"}]}
      :path==="/api/control/missions"&&new URL(route.request().url()).searchParams.get("project")==="test"?[mission]
      :path==="/api/control/missions/44584615-b118-45c4-a4e7-299c2ab1a153"?mission
      :path.endsWith("/events")?events
      :path==="/api/control/stream"?undefined
      :path.endsWith("/files")?{entries:[]}
      :path.endsWith("/crons")?{jobs:[]}
      :path.endsWith("/controller")?{job:null,runs:[]}
      :[];
    if(path==="/api/control/stream")return route.fulfill({contentType:"text/event-stream",body:""});
    return route.fulfill({json});
  });
  await page.goto("/");
  await page.getByRole("button",{name:"test",exact:true}).click();
  await page.getByRole("button",{name:"1 finished"}).click();
  await page.getByRole("button",{name:/Orb native Grok launch verification/}).click();
  await expect(page.locator(".tb-title")).toContainText("Orb native Grok launch verification");
  await expect(page.locator(".tb-title")).not.toHaveText(/^Mission$/);
  await expect(page.locator(".st-text")).toHaveCount(1);
  await expect(page.locator(".st-text")).toContainText("ORB_PROD_NATIVE_GROK_OK");
  await expect(page.locator(".st-text")).toContainText("spark-de79");
  await page.screenshot({path:"test-results/orb-native-canary-once.png"});
});

test("legacy OpenCode results render as Markdown with the raw log folded away",async({page})=>{
 await page.goto("/tests/transcript.html");await page.waitForFunction(()=>!!(window as any).transcriptHarness);
 await page.evaluate(()=>{
  const raw="Remote node 'dgx-spark' job 3dff58d2-508c-458e-90c1-701e402a6b5f finished with state 'succeeded' (exit Some(0))\n\nlog tail:\ntruncated first line...\n"+JSON.stringify({type:"text",sessionID:"ses_native",part:{id:"part_1",text:"## Status\n\nRunning and durable."}});
  (window as any).transcriptHarness.reset([{type:"assistant_message",data:{content:raw}}]);
 });
 await expect(page.getByRole("heading",{name:"Status"})).toBeVisible();await expect(page.locator(".legacy-log pre")).toBeHidden();
 await page.getByText("Original execution log").click();await expect(page.locator(".legacy-log pre")).toContainText('"sessionID":"ses_native"');
});
