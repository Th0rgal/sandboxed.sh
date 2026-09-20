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
