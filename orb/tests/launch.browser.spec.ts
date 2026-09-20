import { test,expect,type Page } from "@playwright/test";
import { writeFileSync } from "node:fs";
const prompt="/goal Check remote startup without losing this draft";
const base={id:"accepted",title:"Remote task",status:"pending",history:[],workspace_name:"host",created_at:"",updated_at:""};
const node={id:"dgx-spark",status:"online",cordoned:false};
async function setup(page:Page, options:{reject?:boolean;missing?:boolean;legacy?:boolean;failed?:boolean;unsupported?:boolean;emptyStatus?:string}={}){
 let posts:any[]=[], releasePost!:()=>void,releaseHistory!:()=>void;
 const postGate=new Promise<void>(resolve=>releasePost=resolve),historyGate=new Promise<void>(resolve=>releaseHistory=resolve);
 let fail=!!options.reject;let fleetReads=0;let listReads=0;
 const m={...base,...(options.failed?{status:options.emptyStatus??"interrupted",goal_mode:true,goal_objective:"Original saved objective",terminal_reason:"orphan_no_runner",remote_node_id:"dgx-spark"}:{})};
 await page.addInitScript(()=>{
  localStorage.setItem("orb.apiUrl",location.origin);localStorage.setItem("orb.jwt","test");localStorage.setItem("orb-theme","dark");
  localStorage.setItem("orb.harnessPick",JSON.stringify({backend:"grok",model:"grok-4.6"}));
  const timing:any={};(window as any).launchTiming=timing;
  document.addEventListener("keydown",event=>{if(event.key==="Enter"&&event.target instanceof HTMLTextAreaElement&&!timing.start)timing.start=performance.now();},true);
  new MutationObserver(()=>{
   const status=document.querySelector(".launch-status"),user=document.querySelector(".launch-preview .user");
   if(timing.start&&user&&status&&!timing.optimistic)timing.optimistic=performance.now()-timing.start;
   if(timing.response&&document.querySelector('textarea[placeholder="Send follow-up"]')&&!timing.acceptedView)timing.acceptedView=performance.now()-timing.response;
  }).observe(document,{subtree:true,childList:true});
  const fetcher=window.fetch.bind(window);window.fetch=async(input,init)=>{const response=await fetcher(input,init);if(String(input).endsWith("/api/control/missions")&&init?.method==="POST"&&response.ok)timing.response=performance.now();return response;};
 });
 await page.route("**/api/**",async route=>{
  const request=route.request(),url=new URL(request.url()),path=url.pathname;
  if(path.includes("proxy-keys"))throw new Error("Frontend must not mint remote credentials");
  if(path==="/api/control/missions"&&request.method()==="POST"){
   posts.push(request.postDataJSON());await postGate;
   if(options.unsupported)return route.fulfill({status:422,body:"Selected remote harness codex is not supported on dgx-spark"});
   if(options.legacy)return route.fulfill({status:400,body:"remote_command is required when remote_node_id is set"});
   if(fail)return route.fulfill({status:503,body:"Runner admission unavailable"});return route.fulfill({json:m});
  }
  if(path==="/api/control/missions"&&!url.searchParams.has("project")){
   listReads++;if(posts.length)await new Promise(r=>setTimeout(r,3000));return route.fulfill({json:options.failed?[m]:[]});
  }
  if(path.endsWith("/events")) {if(!options.failed)await historyGate;return route.fulfill({json:options.failed?[]:[{id:1,event_id:"initial",sequence:1,event_type:"user_message",content:prompt,timestamp:""}]});}
  if(path==="/api/control/stream"){await historyGate;if(options.failed)return route.fulfill({contentType:"text/event-stream",body:""});return route.fulfill({contentType:"text/event-stream",body:`event: user_message\ndata: ${JSON.stringify({id:"initial",content:prompt})}\n\n`});}
  if(path==="/api/control/missions/accepted")return route.fulfill({json:m});
  const json=path==="/api/projects"?{projects:[{slug:"test",title:"Test"}]}:path==="/api/backends"?[{id:"grok",name:"Grok"},{id:"codex",name:"Codex"}]:path==="/api/providers/backend-models"?{backends:{grok:[{value:"grok-4.6",label:"Grok 4.6"}],codex:[{value:"codex-model",label:"Codex model"}]}}:path==="/api/remote-nodes"?{enabled:true,nodes:options.missing&&++fleetReads>1?[]:[node]}:path==="/api/control/missions"?options.failed?[m]:[]:path.endsWith("/files")?{entries:[]}:path.endsWith("/crons")?{jobs:[]}:{job:null,runs:[]};
  return route.fulfill({json});
 });
 await page.goto("/");
 await expect(page.getByRole("button",{name:"Grok",exact:true})).toBeVisible();
 return {posts,releasePost,releaseHistory,setSuccess:()=>{fail=false;},listReads:()=>listReads};
}
async function chooseRemote(page:Page){await page.getByRole("button",{name:/Core \(agent-core\)/}).click();await page.getByRole("button",{name:/dgx-spark online/}).click();}

test("slow remote POST shows prompt immediately; accepted mission opens before slow list refresh and reconciles history",async({page})=>{
 const state=await setup(page);await chooseRemote(page);
 const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");
 await expect(page.locator(".launch-preview .user")).toHaveText(prompt);await expect(page.getByRole("status")).toContainText("Starting on DGX Spark");
 await input.dispatchEvent("keydown",{key:"Enter"});await expect.poll(()=>state.posts.length).toBe(1);
 await page.screenshot({path:"test-results/orb-launch-starting.png"});
 await page.emulateMedia({reducedMotion:"reduce"});await expect(page.locator(".launch-pulse")).toHaveCSS("animation-name","none");
 await page.waitForTimeout(1000);state.releasePost();await expect(page.getByPlaceholder("Send follow-up")).toBeVisible({timeout:1500});
 await expect(page.getByRole("status")).toContainText("Queued on DGX Spark");await expect(page.locator(".user")).toHaveCount(1);await expect(page.locator(".user")).toHaveText(prompt);
 expect(state.posts[0]).toMatchObject({remote_node_id:"dgx-spark",backend:"grok",model_override:"grok-4.6",prompt});expect(state.posts[0]).not.toHaveProperty("remote_command");expect(state.posts[0].idempotency_key).toBeTruthy();
 state.releaseHistory();await expect(page.locator(".user")).toHaveCount(1);await expect(page.locator(".user")).toHaveText(prompt);
 const timings=await page.evaluate(()=>(window as any).launchTiming);console.log("LAUNCH_TIMING",JSON.stringify(timings));expect(timings.optimistic).toBeLessThan(500);expect(timings.acceptedView).toBeLessThan(500);writeFileSync("test-results/launch-timings.json",JSON.stringify(timings,null,2));
});

test("rejection preserves draft; explicit retry uses the same idempotency key",async({page})=>{
 const state=await setup(page,{reject:true});await chooseRemote(page);state.releasePost();const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("Runner admission unavailable");await expect(input).toHaveValue(prompt);expect(state.posts).toHaveLength(1);
 state.setSuccess();await input.press("Enter");await expect(page.getByPlaceholder("Send follow-up")).toBeVisible();expect(state.posts).toHaveLength(2);expect(state.posts[1].idempotency_key).toBe(state.posts[0].idempotency_key);
});

test("missing selected node never silently launches on Core",async({page})=>{
 const state=await setup(page,{missing:true});await chooseRemote(page);const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");await expect(page.getByRole("alert")).toContainText("DGX Spark is unavailable");await expect(input).toHaveValue(prompt);expect(state.posts).toHaveLength(0);
});

test("legacy remote contract explains unsupported launch without shell fallback",async({page})=>{
 const state=await setup(page,{legacy:true});state.releasePost();await chooseRemote(page);const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");await expect(page.getByRole("alert")).toContainText("backend needs an update");await expect(input).toHaveValue(prompt);expect(state.posts).toHaveLength(1);
});

test("empty failed mission shows recovered saved goal and honest terminal status",async({page})=>{
 await setup(page,{failed:true});await page.getByRole("button",{name:"Test",exact:true}).click();await page.getByRole("button",{name:/1 finished/}).click();await page.getByRole("button",{name:/Remote task/}).click();
 await expect(page.getByRole("status")).toContainText("Interrupted on DGX Spark");await expect(page.getByRole("status")).toContainText("could not find an active runner");await expect(page.locator(".user")).toHaveText("/goal Original saved objective");await expect(page.locator(".launch-pulse")).toHaveCount(0);await page.screenshot({path:"test-results/orb-launch-interrupted.png"});
});

test("unsupported remote harness is explicit and never changed to Claude",async({page})=>{
 const state=await setup(page,{unsupported:true});state.releasePost();await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:"Codex 1",exact:true}).click();
 const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("codex is not supported");await expect(input).toHaveValue(prompt);
 expect(state.posts).toHaveLength(1);expect(state.posts[0]).toMatchObject({backend:"codex",model_override:"codex-model",remote_node_id:"dgx-spark"});
});

test("startup timing benchmark: repeated explicit launches get distinct request identities",async({page})=>{
 const state=await setup(page);state.releasePost();state.releaseHistory();await chooseRemote(page);
 const samples:{optimistic:number;acceptedView:number}[]=[];
 for(let i=0;i<12;i++){
  if(i)await page.getByRole("button",{name:/New Agent/}).click();
  await page.evaluate(()=>{for(const key of Object.keys((window as any).launchTiming))delete (window as any).launchTiming[key];});
  const input=page.getByPlaceholder("Plan, Build, / for commands, @ for context");await input.fill(prompt);await input.press("Enter");
  await expect(page.getByPlaceholder("Send follow-up")).toBeVisible();
  samples.push(await page.evaluate(()=>{const {optimistic,acceptedView}=(window as any).launchTiming;return {optimistic,acceptedView};}));
 }
 const percentile=(key:"optimistic"|"acceptedView",q:number)=>samples.map(s=>s[key]).sort((a,b)=>a-b)[Math.ceil(samples.length*q)-1];
 const result={samples,optimisticMedian:percentile("optimistic",.5),optimisticP95:percentile("optimistic",.95),acceptedViewMedian:percentile("acceptedView",.5),acceptedViewP95:percentile("acceptedView",.95)};
 console.log("STARTUP_BENCHMARK",JSON.stringify(result));writeFileSync("test-results/launch-benchmark.json",JSON.stringify(result,null,2));
 expect(result.optimisticP95).toBeLessThan(500);expect(result.acceptedViewP95).toBeLessThan(500);expect(new Set(state.posts.map(p=>p.idempotency_key)).size).toBe(12);
});

for(const status of ["failed","resuming"])test(`empty ${status} mission retains goal and exposes status`,async({page})=>{
 await setup(page,{failed:true,emptyStatus:status});await page.getByRole("button",{name:"Test",exact:true}).click();
 if(status==="failed")await page.getByRole("button",{name:/1 finished/}).click();
 await page.getByRole("button",{name:/Remote task/}).click();
 await expect(page.getByRole("status")).toContainText(status==="failed"?"Failed on DGX Spark":"Resuming on DGX Spark");
 await expect(page.locator(".user")).toHaveText("/goal Original saved objective");
 await expect(page.locator(".launch-pulse")).toHaveCount(status==="failed"?0:1);
});
