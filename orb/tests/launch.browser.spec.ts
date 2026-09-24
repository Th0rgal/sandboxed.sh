import { test,expect,type Page } from "@playwright/test";
import { writeFileSync } from "node:fs";
const objective="Check remote startup without losing this draft";
const prompt=`/goal ${objective}`;
const base={id:"accepted",title:"Remote task",status:"pending",history:[],workspace_name:"host",backend:"grok",model_override:"grok-4.6",created_at:"",updated_at:""};
const node={id:"dgx-spark",status:"online",cordoned:false};
// What production (release 21e29373) advertises today. Grok appears only when a
// backend that supports native Grok remote launches says so.
type Capability={typed?:boolean;harnesses?:string[];raw_command?:boolean;proxy_url_configured?:boolean;requires_proxy_harnesses?:string[]};
const typedCapability:Capability={typed:true,harnesses:["claudecode","opencode"],raw_command:true,proxy_url_configured:true};
async function setup(page:Page, options:{reject?:boolean;legacy?:boolean;remoteSuccess?:boolean;missing?:boolean;failed?:boolean;remoteJob?:{phase:string;node_state?:string};emptyStatus?:string;capability?:Capability|null;fleetFailAfterFirst?:boolean;files?:{name:string;kind:string}[];emptyContext?:boolean}={}){
 let attachmentReads:string[]=[];
 let posts:any[]=[], releasePost!:()=>void,releaseHistory!:()=>void;
 const postGate=new Promise<void>(resolve=>releasePost=resolve),historyGate=new Promise<void>(resolve=>releaseHistory=resolve);
 let fail=!!options.reject;let fleetReads=0;let listReads=0;
 const capability=options.capability===undefined?typedCapability:options.capability;
 const m={...base,...(options.remoteSuccess?{status:"active",remote_job:{job_id:"job-123",node_id:"dgx-spark",phase:"observed"},execution:{state:"waiting_remote_job"}}:{}),...(options.remoteJob?{remote_job:{job_id:"job-123",node_id:"dgx-spark",...options.remoteJob},execution:{state:"waiting_remote_job"}}:{}),...(options.failed?{status:options.emptyStatus??"interrupted",goal_mode:true,goal_objective:"Original saved objective",terminal_reason:"orphan_no_runner",remote_node_id:"dgx-spark"}:{})};
 await page.addInitScript(()=>{
  localStorage.setItem("orb.apiUrl",location.origin);localStorage.setItem("orb.jwt","test");localStorage.setItem("orb-theme","dark");
  localStorage.setItem("orb.harnessPick",JSON.stringify({backend:"grok",model:"grok-4.6"}));
  const timing:any={};(window as any).launchTiming=timing;
  document.addEventListener("keydown",event=>{if(event.key==="Enter"&&event.target instanceof HTMLTextAreaElement&&!timing.start)timing.start=performance.now();},true);
  new MutationObserver(()=>{
   const status=document.querySelector(".launch-preview .user.pending"),user=document.querySelector(".launch-preview .user");
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
   if(options.legacy)return route.fulfill({status:400,body:"remote_command is required when remote_node_id is set"});
   if(request.postDataJSON().remote_node_id && !options.remoteSuccess)return route.fulfill({status:400,body:`REMOTE_HARNESS_UNSUPPORTED: backend '${request.postDataJSON().backend}' cannot run on remote nodes`});
   if(fail)return route.fulfill({status:503,body:"Runner admission unavailable"});return route.fulfill({json:m});
  }
  if(path==="/api/control/missions"&&!url.searchParams.has("project")){
   listReads++;if(posts.length)await new Promise(r=>setTimeout(r,3000));return route.fulfill({json:options.failed?[m]:[]});
  }
  if(path==="/api/control/queue")return route.fulfill({json:[]});
  if(path.endsWith("/events")) {if(!options.failed)await historyGate;return route.fulfill({json:options.failed?[]:[{id:1,event_id:"initial",sequence:1,event_type:"user_message",content:prompt,timestamp:""}]});}
  if(path==="/api/control/stream"){await historyGate;if(options.failed)return route.fulfill({contentType:"text/event-stream",body:""});return route.fulfill({contentType:"text/event-stream",body:`event: user_message\ndata: ${JSON.stringify({id:"initial",content:prompt})}\n\n`});}
  if(path==="/api/control/missions/accepted")return route.fulfill({json:m});
  if(path==="/api/remote-nodes"){
   fleetReads++;
   if(options.fleetFailAfterFirst&&fleetReads>1)return route.fulfill({status:503,body:"fleet monitor unavailable"});
   return route.fulfill({json:{enabled:true,nodes:options.missing&&fleetReads>1?[]:[node],...(capability?{remote_launch:capability}:{})}});
  }
  if(path.endsWith("/files") || path.includes("/controller")) {
   attachmentReads.push(path);
   if(!path.startsWith("/api/projects/test/")) return route.fulfill({status:404,body:"Unknown project"});
   if(options.emptyContext) return route.fulfill({json:path.endsWith("/files")?{entries:[]}:{job:null,runs:[]}});
  }
  const filePath=url.searchParams.get("path")||"";
  const json=path==="/api/projects"?{projects:[{slug:"test",title:"Test"}]}:path==="/api/backends"?[{id:"grok",name:"Grok"},{id:"codex",name:"Codex"},{id:"opencode",name:"OpenCode"},{id:"claudecode",name:"Claude Code"}]:path==="/api/providers/backend-models"?{backends:{grok:[{value:"grok-4.6",label:"Grok 4.6"}],codex:[{value:"codex-model",label:"Codex model"}],opencode:[{value:"xai/grok-4.6",label:"Grok 4.6"}],claudecode:[{value:"claude-sonnet-4-6",label:"Claude Sonnet 4.6"}]}}:path==="/api/control/missions"?options.failed?[m]:[]:path.endsWith("/files")?{entries:filePath==="notes"?[{name:"foo.md",kind:"file"}]:(options.files??[])}:path.endsWith("/crons")?{jobs:[]}:{job:path.includes("/controller")?{id:"ctrl-1",name:"Test controller",enabled:true,state:"scheduled"}:null,runs:[]};
  return route.fulfill({json});
 });
 await page.goto("/");
 await expect(page.getByRole("button",{name:"Grok",exact:true})).toBeVisible();
 // These scenarios exercise the named project's context; new sessions now default to Default.
 if (!options.failed) {
  await page.getByRole("button",{name:"Choose project",exact:true}).click();
  await page.getByRole("option",{name:"Test",exact:true}).click();
 }
 return {posts,attachmentReads,releasePost,releaseHistory,setSuccess:()=>{fail=false;},listReads:()=>listReads,fleetReads:()=>fleetReads};
}
async function chooseRemote(page:Page){await page.getByRole("button",{name:/Core \(agent-core\)/}).click();await page.getByRole("button",{name:/dgx-spark online/}).click();}
const composerInput=(page:Page)=>page.getByPlaceholder(/Describe a task, \/ for commands, @ for context|Describe the objective/);
/**
 * Where a phase is reported. A healthy in-flight mission no longer draws a
 * banner — the prompt animates and the phase is announced in a visually hidden
 * live region — so assert against both, and use `.launch-status` directly only
 * where the test is specifically about the visible banner.
 */
const phaseStatus=(page:Page)=>page.locator(".launch-status, .sr-only[role=status]");
/** A `/goal` turn renders as a Goal tag plus the exact objective, never the raw slash command. */
async function expectGoalTurn(page:Page,selector:string,text=objective){
 const turn=page.locator(`${selector}:not(.sk-user)`);await expect(turn).toHaveCount(1);
 await expect(turn).toHaveClass(/goal/);await expect(turn.locator(".goal-tag")).toHaveCount(0);await expect(turn.locator(":scope > span:last-child")).toHaveText(text);
}

test("slow local POST shows prompt immediately; accepted mission opens before slow list refresh and reconciles history",async({page})=>{
 const state=await setup(page);
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expectGoalTurn(page,".launch-preview .user");await expect(phaseStatus(page)).toContainText("Starting on Core");
 await input.dispatchEvent("keydown",{key:"Enter"});await expect.poll(()=>state.posts.length).toBe(1);
 await page.screenshot({path:"test-results/orb-launch-starting.png"});
 // The working indicator is the prompt itself now, and it must not move when
 // the user has asked for reduced motion.
 await page.emulateMedia({reducedMotion:"reduce"});await expect(page.locator(".user.pending")).toHaveCSS("animation-name","none");
 const previewBox = await page.locator(".launch-preview .user").boundingBox();
 await page.waitForTimeout(1000);state.releasePost();await expect(page.getByPlaceholder("Send follow-up")).toBeVisible({timeout:1500});
 await expectGoalTurn(page,".user");
 await expect(page.locator(".sk-transcript")).toHaveCount(0);
 await expect(page.getByPlaceholder("Send follow-up")).toBeVisible();
 expect(state.posts[0]).toMatchObject({backend:"grok",model_override:"grok-4.6",prompt,title:"Check remote startup without losing this…"});expect(state.posts[0]).not.toHaveProperty("remote_node_id");expect(state.posts[0]).not.toHaveProperty("remote_command");expect(state.posts[0].idempotency_key).toBeTruthy();
 const acceptedBox = await page.locator(".user").boundingBox();
 expect(Math.abs(acceptedBox!.y - previewBox!.y)).toBeLessThan(3);
 expect(Math.abs(acceptedBox!.x - previewBox!.x)).toBeLessThan(3);
 state.releaseHistory();await expectGoalTurn(page,".user");
 await expect(page.locator(".new-agent")).toHaveCount(0);
 const timings=await page.evaluate(()=>(window as any).launchTiming);console.log("LAUNCH_TIMING",JSON.stringify(timings));expect(timings.optimistic).toBeLessThan(500);expect(timings.acceptedView).toBeLessThan(500);writeFileSync("test-results/launch-timings.json",JSON.stringify(timings,null,2));
});

test("/goal draft shows a Goal indicator, needs an objective, and is sent as the canonical goal prompt with an objective title",async({page})=>{
 const state=await setup(page);state.releasePost();state.releaseHistory();
 const input=composerInput(page);const chip=page.locator(".composer .mode-chip");
 await input.pressSequentially("/");
 await expect(page.locator(".slash-menu")).toContainText("Goal");
 await input.press("Enter");
 await expect(page.locator(".slash-menu")).toHaveCount(0);
 await expect(chip).toHaveAttribute("role","status");await expect(chip).toContainText("Goal");
 await expect(input).toHaveValue("");
 await input.press("Enter");await expect(page.getByRole("alert")).toContainText("Add an objective after /goal");expect(state.posts).toHaveLength(0);await expect(input).toHaveValue("");await expect(chip).toBeVisible();await expect(input).toBeFocused();
 await chip.getByRole("button",{name:"Remove Goal"}).click();
 await expect(chip).toHaveCount(0);
 await input.fill("/goals are nice");await expect(chip).toHaveCount(0);
 await input.fill(`  /goal   ${objective}`);await expect(chip).toContainText("Goal");
 await expect(input).toHaveValue(objective);
 await expect(page.getByRole("alert")).toHaveCount(0);
 await expect(chip).toHaveAttribute("aria-label",/keeps iterating/);
 const align=await page.evaluate(()=>{
  const tag=document.querySelector(".composer .mode-chip");
  const field=document.querySelector(".composer-field");
  const ta=document.querySelector(".composer textarea");
  if(!tag||!field||!ta)return null;
  const t=tag.getBoundingClientRect(),f=field.getBoundingClientRect(),a=ta.getBoundingClientRect();
  return {tagH:t.height,inField:t.left>=f.left&&t.right<=f.right+1,topDelta:Math.abs(t.top-a.top)};
 });
 expect(align).not.toBeNull();
 expect(align!.tagH).toBe(24);
 expect(align!.inField).toBe(true);
 expect(align!.topDelta).toBeLessThan(4);
 await input.press("Tab");expect(await page.evaluate(()=>!!document.activeElement?.closest(".mode-chip"))).toBe(false);
 await page.screenshot({path:"test-results/orb-goal-composer.png"});
 await input.focus();await input.press("Enter");
 await expect(page.getByPlaceholder("Send follow-up")).toBeVisible();
 await expect(page.locator(".under-harness")).toHaveText("Grok");
 await expect(page.locator(".under-model")).toHaveText("4.6");
 expect(state.posts).toHaveLength(1);
 expect(state.posts[0]).toMatchObject({prompt,title:"Check remote startup without losing this…",backend:"grok",model_override:"grok-4.6"});
 expect(state.posts[0]).not.toHaveProperty("goal_mode");expect(state.posts[0]).not.toHaveProperty("goal_objective");
 await expectGoalTurn(page,".user");
 await expect(page.locator(".launch-status .goal-tag")).toHaveText("Goal");await expect(page.locator(".launch-status")).toContainText("Queued on Core");
 await expect(page.locator(".launch-status .goal-tag")).toHaveCount(1);
 await page.screenshot({path:"test-results/orb-goal-accepted.png"});
 await page.emulateMedia({colorScheme:"light"});await page.evaluate(()=>{localStorage.setItem("orb-theme","light");document.documentElement.dataset.theme="light";});
 await page.screenshot({path:"test-results/orb-goal-accepted-light.png"});
});

test("goal indicator overhead per keystroke stays negligible",async({page})=>{
 await setup(page);await composerInput(page).click();
 const result=await page.evaluate(()=>{
  const ta=document.querySelector("textarea")!;
  const measure=(text:string,chip:boolean)=>{
   const samples:number[]=[];
   for(let i=0;i<60;i++){
    ta.value="";ta.dispatchEvent(new Event("input",{bubbles:true}));document.body.offsetHeight;
    const start=performance.now();ta.value=`${text}${i}`;ta.dispatchEvent(new Event("input",{bubbles:true}));document.body.offsetHeight;
    samples.push(performance.now()-start);
    if(!!document.querySelector(".composer .mode-chip")!==chip)throw new Error(`indicator mismatch for ${text}`);
   }
   samples.sort((a,b)=>a-b);return {median:samples[29],p95:samples[56],max:samples[59]};
  };
  return {plain:measure("Plan the release ",false),goal:measure("/goal Plan the release ",true),goalEdit:measure("/goal Plan the release and ship it ",true)};
 });
 console.log("GOAL_COMPOSER_TIMING",JSON.stringify(result));writeFileSync("test-results/goal-composer-timings.json",JSON.stringify(result,null,2));
 expect(result.goal.p95).toBeLessThan(50);expect(result.goalEdit.p95).toBeLessThan(50);
});

 test("rejection preserves draft; explicit retry uses the same idempotency key",async({page})=>{
  const state=await setup(page,{reject:true});state.releasePost();const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
  await expect(page.getByRole("alert")).toContainText("Runner admission unavailable");await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(1);
  await input.fill(`${prompt} still`);await expect(page.getByRole("alert")).toContainText("Runner admission unavailable");
  await input.fill(prompt);state.setSuccess();await input.press("Enter");await expect(page.getByPlaceholder("Send follow-up")).toBeVisible();expect(state.posts).toHaveLength(2);expect(state.posts[1].idempotency_key).toBe(state.posts[0].idempotency_key);
 });

test("missing selected node never silently launches on Core",async({page})=>{
 const state=await setup(page,{missing:true});await chooseRemote(page);const input=composerInput(page);await input.fill(prompt);await input.press("Enter");await expect(page.getByRole("alert")).toContainText("DGX Spark is unavailable");await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(0);
});

test("preflight refuses Grok on a node whose server only advertises Claude Code and OpenCode, keeping the exact selection and draft",async({page})=>{
 const state=await setup(page);state.releasePost();await chooseRemote(page);
 await expect(page.getByRole("button",{name:/DGX Spark/})).toBeVisible();
 await page.getByRole("button",{name:"Grok",exact:true}).click();
 const menu=page.locator(".picks .menu");
 await expect(menu.getByRole("button",{name:/^Grok/})).toContainText("not on DGX Spark");
 await expect(menu.getByRole("button",{name:/^Codex/})).toContainText("not on DGX Spark");
 await expect(menu.getByRole("button",{name:"Claude Code 1",exact:true})).toBeVisible();
 await expect(menu.getByRole("button",{name:"OpenCode 1",exact:true})).toBeVisible();
 await page.screenshot({path:"test-results/orb-harness-menu-remote.png"});
 await page.keyboard.press("Escape");await expect(menu).toHaveCount(0);
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("Remote launch for grok (grok-4.6) is not supported on dgx-spark");
 await expect(page.getByRole("alert")).toContainText("Claude Code, OpenCode");
 await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(0);
 await expect(page.getByRole("button",{name:"Grok",exact:true})).toBeVisible();
 await expect(page.getByRole("button",{name:/DGX Spark/})).toBeVisible();
 await expect(page.getByPlaceholder("Send follow-up")).toHaveCount(0);
 await page.screenshot({path:"test-results/orb-remote-unsupported.png"});
});

test("Grok remote launch is sent unchanged once the server advertises grok",async({page})=>{
 const state=await setup(page,{remoteSuccess:true,capability:{...typedCapability,harnesses:["claudecode","opencode","grok"]}});
 await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();
 const menu=page.locator(".picks .menu");await expect(menu.getByRole("button",{name:/^Grok 1/})).toBeVisible();await expect(menu.getByRole("button",{name:/^Codex/})).toContainText("not on DGX Spark");
 await page.keyboard.press("Escape");
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expectGoalTurn(page,".launch-preview .user");await expect(phaseStatus(page)).toContainText("Starting on DGX Spark");
 await expect.poll(()=>state.posts.length).toBe(1);
 expect(state.posts[0]).toMatchObject({backend:"grok",model_override:"grok-4.6",remote_node_id:"dgx-spark",prompt,title:"Check remote startup without losing this…"});
 expect(state.posts[0]).not.toHaveProperty("remote_command");
 state.releasePost();await expect(page.getByPlaceholder("Send follow-up")).toBeVisible({timeout:1500});
 await expect(page.locator(".under-loc")).toContainText("DGX Spark");
 await expect(page.locator(".under-harness")).toHaveText("Grok");
 await expect(page.locator(".under-model")).toHaveText("4.6");
 await expect(phaseStatus(page)).toContainText("Remote job accepted on DGX Spark");
 // Without transcript output or running evidence, acceptance stays explicit.
 await expect(page.locator(".launch-status")).toContainText("Remote job accepted");
 await expectGoalTurn(page,".user");state.releaseHistory();await expectGoalTurn(page,".user");
 await page.screenshot({path:"test-results/orb-remote-grok-accepted.png"});
});

test("server without the remote_launch capability is refused before POST and shown in the menus",async({page})=>{
 const state=await setup(page,{capability:null});state.releasePost();await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();
 await expect(page.locator(".picks .menu").getByRole("button",{name:/^Claude Code/})).toContainText("no typed remote launch");
 await page.getByRole("button",{name:/^Claude Code/}).click();
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("does not support structured remote launches");
 await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(0);
 await page.getByRole("button",{name:/DGX Spark/}).click();
 await expect(page.getByRole("button",{name:/dgx-spark online/})).toContainText("no typed remote launch");
});

test("capability read failure refuses before POST and reports support as last known",async({page})=>{
 const state=await setup(page,{fleetFailAfterFirst:true});state.releasePost();await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:"Claude Code 1",exact:true}).click();
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("Could not confirm remote launch support on DGX Spark");
 await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(0);
 await page.getByRole("button",{name:/DGX Spark/}).click();
 await expect(page.getByRole("button",{name:/dgx-spark online/})).toContainText("Claude Code, OpenCode (last known)");
});

 test("proxy URL not configured still launches native Grok OAuth",async({page})=>{
  const noProxy={...typedCapability,harnesses:["claudecode","opencode","grok"],proxy_url_configured:false};
  const state=await setup(page,{remoteSuccess:true,capability:noProxy});state.releasePost();await chooseRemote(page);
  const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
  await expect.poll(()=>state.posts.length).toBe(1);
  expect(state.posts[0]).toMatchObject({backend:"grok",model_override:"grok-4.6",remote_node_id:"dgx-spark"});
  await expect(page.getByRole("alert")).toHaveCount(0);
 });

 test("proxy URL not configured blocks Claude Code before POST without env jargon",async({page})=>{
  const noProxy={...typedCapability,harnesses:["claudecode","opencode","grok"],proxy_url_configured:false};
  const state=await setup(page,{capability:noProxy});state.releasePost();await chooseRemote(page);
  await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:"Claude Code 1",exact:true}).click();
  const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
  await expect(page.getByRole("alert")).toContainText("cannot reach this backend's model proxy");
  await expect(page.getByRole("alert")).not.toContainText("SANDBOXED_PUBLIC_URL");
  await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(0);
  await page.screenshot({path:"test-results/orb-remote-proxy-missing.png"});
 });

test("empty failed mission shows recovered saved goal and honest terminal status",async({page})=>{
 await setup(page,{failed:true});await page.getByRole("button",{name:"Test",exact:true}).click();await page.getByRole("button",{name:/1 finished/}).click();await page.getByRole("button",{name:/Remote task/}).click();
 await expect(page.locator(".launch-status")).toContainText("Interrupted on DGX Spark");await expect(page.locator(".launch-status")).toContainText("could not find an active runner");
 await expectGoalTurn(page,".user","Original saved objective");await expect(page.locator(".launch-status .goal-tag")).toHaveText("Goal");await expect(page.locator(".tb-title .goal-tag")).toHaveText("Goal");
 await expect(page.locator(".launch-pulse")).toHaveCount(0);await page.screenshot({path:"test-results/orb-launch-interrupted.png"});
});

test("unsupported remote harness is explicit and never changed to Claude",async({page})=>{
 const state=await setup(page);state.releasePost();await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:/^Codex/}).click();
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("codex (codex-model) is not supported");await expect(input).toHaveValue(objective);
 expect(state.posts).toHaveLength(0);
});

test("startup timing benchmark: repeated explicit launches get distinct request identities",async({page})=>{
 const state=await setup(page);state.releasePost();state.releaseHistory();
 const samples:{optimistic:number;acceptedView:number}[]=[];
 for(let i=0;i<12;i++){
  if(i)await page.getByRole("button",{name:/New Agent/}).click();
  await page.evaluate(()=>{for(const key of Object.keys((window as any).launchTiming))delete (window as any).launchTiming[key];});
  const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
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
 await expect(phaseStatus(page)).toContainText(status==="failed"?"Failed on DGX Spark":"Resuming on DGX Spark");
 await expectGoalTurn(page,".user","Original saved objective");
 await expect(page.locator(status==="failed"?".launch-pulse":".user.pending")).toHaveCount(status==="failed"?0:1);
});

for(const [phase,node_state,label] of [["observed",undefined,"Remote job accepted"],["observed","queued","Queued"],["observed","running","Running"],["unobserved",undefined,"Checking remote job"],["submit_ambiguous",undefined,"Checking submission"]] as const)test(`Active remote mission shows ${label} from durable job evidence`,async({page})=>{
 await setup(page,{failed:true,emptyStatus:"active",remoteJob:{phase,node_state}});
 await page.getByRole("button",{name:"Test",exact:true}).click();
 await page.getByRole("button",{name:/Remote task/}).click();
 await expect(phaseStatus(page)).toContainText(`${label} on DGX Spark`);
 await expectGoalTurn(page,".user","Original saved objective");
});

for(const harness of ["claudecode", "opencode"] as const)test(`supported remote ${harness} slow POST preserves selection and opens durable job immediately`,async({page})=>{
 const state=await setup(page,{remoteSuccess:true});await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:harness === "claudecode" ? "Claude Code 1" : "OpenCode 1",exact:true}).click();
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expectGoalTurn(page,".launch-preview .user");
 await expect(phaseStatus(page)).toContainText("Starting on DGX Spark");
 await expect.poll(()=>state.posts.length).toBe(1);
 expect(state.posts[0]).toMatchObject({backend:harness,model_override:harness === "claudecode" ? "claude-sonnet-4-6" : "xai/grok-4.6",remote_node_id:"dgx-spark",prompt});
 expect(state.posts[0]).not.toHaveProperty("remote_command");expect(state.posts[0]).not.toHaveProperty("remote_async");
 await input.dispatchEvent("keydown",{key:"Enter"});
 await page.waitForTimeout(1000);expect(state.posts).toHaveLength(1);
 await expectGoalTurn(page,".launch-preview .user");
 state.releasePost();await expect(page.getByPlaceholder("Send follow-up")).toBeVisible({timeout:1500});
 await expect(phaseStatus(page)).toContainText("Remote job accepted on DGX Spark");
 await expectGoalTurn(page,".user");state.releaseHistory();await expectGoalTurn(page,".user");
 await page.screenshot({path:`test-results/orb-remote-${harness}-accepted.png`});
});

test("typed-capable server that still answers remote_command required is explained without retry or fallback",async({page})=>{
 const state=await setup(page,{legacy:true});state.releasePost();await chooseRemote(page);
 await page.getByRole("button",{name:"Grok",exact:true}).click();await page.getByRole("button",{name:"Claude Code 1",exact:true}).click();
 const input=composerInput(page);await input.fill(prompt);await input.press("Enter");
 await expect(page.getByRole("alert")).toContainText("does not support structured remote launches");
 await expect(input).toHaveValue(objective);expect(state.posts).toHaveLength(1);
 expect(state.posts[0]).toMatchObject({backend:"claudecode",model_override:"claude-sonnet-4-6",remote_node_id:"dgx-spark"});
 expect(state.posts[0]).not.toHaveProperty("remote_command");
});

test("@ file and controller mentions are sent as structured attachments",async({page})=>{
 const state=await setup(page,{files:[{name:"notes",kind:"dir"}]});
 state.releasePost();state.releaseHistory();
 const input=composerInput(page);
 // Mentions are written into the sentence, so the draft is built up rather
 // than replaced: each reference stays where the user put it, and the agent
 // reads the same words the user wrote.
 await input.pressSequentially("Read ");
 await input.pressSequentially("@notes/foo");
 await expect(page.getByRole("listbox",{name:"Context"})).toBeVisible();
 await page.getByRole("option",{name:"notes/foo.md",exact:true}).click();
 await input.pressSequentially("and check @controller");
 await page.getByRole("option",{name:"Test controller",exact:true}).click();
 await expect(input).toHaveValue("Read @notes/foo.md and check @controller ");
 await page.screenshot({path:"artifacts/orb-attachments.png", fullPage:true});
 await input.press("Enter");
 await expect.poll(()=>state.posts.length).toBe(1);
 expect(state.posts[0].attachments).toEqual(expect.arrayContaining([
  {kind:"file",path:"notes/foo.md"},
  {kind:"controller"},
 ]));
 expect(state.posts[0].prompt).toBe("Read @notes/foo.md and check @controller");
 expect(JSON.stringify(state.posts[0])).not.toContain("hello notes");
});


test("new agent uses the same real project for initial context and launch",async({page})=>{
 const state=await setup(page,{files:[{name:"notes",kind:"dir"}]});
 state.releasePost();state.releaseHistory();
 await expect(page.getByRole("button",{name:"Choose project"})).toHaveText(/Test/);
 const input=composerInput(page);
 await input.pressSequentially("Read ");await input.pressSequentially("@foo");
 await page.getByRole("option",{name:"notes/foo.md",exact:true}).click();
 expect(state.attachmentReads.length).toBeGreaterThan(0);
 expect(state.attachmentReads.every(path=>path.startsWith("/api/projects/test/"))).toBe(true);
 // The mention stays in the sentence; the draft is extended, not replaced.
 await input.pressSequentially("for context");await input.press("Enter");
 await expect.poll(()=>state.posts.length).toBe(1);
 expect(state.posts[0]).toMatchObject({project:"test",attachments:[{kind:"file",path:"notes/foo.md"}]});
});

test("connected empty context never substitutes demo attachment items",async({page})=>{
 const state=await setup(page,{emptyContext:true});state.releaseHistory();
 const input=composerInput(page);await input.fill("@");
 await expect.poll(()=>state.attachmentReads.filter(path=>path.endsWith("/files")).length).toBeGreaterThan(0);
 await expect(page.getByRole("listbox",{name:"Context"}).getByRole("option")).toHaveCount(0);
 await page.getByTitle("Add context").click();
 await expect(page.locator(".plus-menu .menu-item")).toHaveCount(0);
 // Nothing was offered, so nothing was written into the draft either.
 await expect(input).toHaveValue("@");
});

test("context picks preserve multiline whitespace and land at the caret",async({page})=>{
 const state=await setup(page,{files:[{name:"notes",kind:"dir"}]});state.releaseHistory();
 const input=composerInput(page);
 await input.fill("Draft:\n\n@foo");
 await page.getByRole("option",{name:"notes/foo.md",exact:true}).click();
 // The blank lines above survive, and the reference replaces exactly the
 // "@foo" that was typed — the caret is left after it, ready to keep writing.
 await expect(input).toHaveValue("Draft:\n\n@notes/foo.md ");
 expect(await input.evaluate(el=>(el as HTMLTextAreaElement).selectionStart)).toBe("Draft:\n\n@notes/foo.md ".length);

 // Mid-sentence: everything to the right of the caret stays to the right.
 await input.fill("Before @foo after");
 await input.evaluate(el=>{const ta=el as HTMLTextAreaElement;ta.focus();ta.setSelectionRange(11,11);ta.dispatchEvent(new Event("click",{bubbles:true}));});
 await expect(page.getByRole("option",{name:"notes/foo.md",exact:true})).toBeVisible();
 await input.press("Enter");
 await expect(input).toHaveValue("Before @notes/foo.md  after");
 expect(await input.evaluate(el=>(el as HTMLTextAreaElement).selectionStart)).toBe("Before @notes/foo.md ".length);
 await input.pressSequentially("inserted");
 await expect(input).toHaveValue("Before @notes/foo.md inserted after");
});

test("disk refusal stays below the draft with readable details and a dismiss action",async({page})=>{
 await setup(page);
 const raw="mission needs an estimated 64 GiB scratch plus a 64 GiB emergency floor (128 GiB required), but only 127 GiB is free at /root (filesystem statvfs:7321850625438636562); select a remote node or free space";
 await page.route("**/api/control/missions",async route=>{
   if(route.request().method()==="POST")return route.fulfill({status:409,body:raw});
   return route.fallback();
 });
 const input=composerInput(page);await input.fill("Summarize the Pareto audit progress");await input.press("Enter");
 const alert=page.getByRole("alert");await expect(alert).toContainText("Not enough disk space");await expect(input).toHaveValue("Summarize the Pareto audit progress");
 const composer=await page.locator(".composer").boundingBox(),notice=await alert.boundingBox();expect(notice!.y).toBeGreaterThanOrEqual(composer!.y+composer!.height);
 await expect(alert.locator("pre")).toBeHidden();await alert.getByText("Technical details").click();await expect(alert.locator("pre")).toHaveText(raw);await alert.getByText("Technical details").click();
 await page.screenshot({path:"test-results/error-notice-dark.png"});
 await page.setViewportSize({width:480,height:850});
 await page.getByRole("button", {name:"Close sidebar", exact:true}).click();
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 await alert.getByRole("button",{name:"Dismiss error"}).click();await expect(alert).toHaveCount(0);await expect(input).toHaveValue("Summarize the Pareto audit progress");
});

test('follow-up composer does not alternate layouts while typing wrapped text',async({page})=>{
 const state=await setup(page);state.releasePost();
 await composerInput(page).fill('hello');await composerInput(page).press('Enter');
 const input=page.getByPlaceholder('Send follow-up');await expect(input).toBeVisible();
 const shapes=await input.evaluate((el)=>{
  const ta=el as HTMLTextAreaElement, box=ta.closest('.composer')!;
  const result:boolean[]=[];
  for(let i=1;i<=280;i++){
   ta.value='suggest a message to send him about the properties '.repeat(8).slice(0,i);
   ta.dispatchEvent(new Event('input',{bubbles:true}));
   result.push(box.classList.contains('tall'));
  }
  return result;
 });
 const first=shapes.indexOf(true);expect(first).toBeGreaterThan(0);
 expect(shapes.slice(first).every(Boolean)).toBe(true);
 await input.fill('short');await expect(page.locator('.composer.tall')).toHaveCount(0);
});
