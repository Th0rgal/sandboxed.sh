import { test, expect, type Page } from "@playwright/test";
import type { StoredEvent } from "../src/stream";

async function setup(page: Page, mixed = false) {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin); localStorage.setItem("orb.jwt", "test"); localStorage.setItem("orb-theme", "dark");
  });
  const mission = {id:"active",title:"Queue and task review",status:"active",project:"test",backend:"codex",history:[],created_at:"",updated_at:""};
  const done = Array.from({length:6},(_,i)=>({...mission,id:`done-${i}`,title:`Finished mission ${i+1}`,status:"completed"}));
  let reject: boolean | string = false, loseReply = false, sequence = 5;
  const pending: Array<{id:string;content:string;mission_id:string}> = [];
  const posts: Array<Record<string,any>> = [];
  const events: StoredEvent[] = [
    {id:1,event_id:"initial",sequence:1,event_type:"user_message",content:"Review this work",timestamp:"",metadata:{queued:false}},
    {id:2,sequence:2,event_type:"tool_call",tool_call_id:"read",tool_name:"Read",content:JSON.stringify({file_path:"src/App.tsx"}),timestamp:""},
    {id:3,sequence:3,event_type:"tool_result",tool_call_id:"read",tool_name:"Read",content:"File contents",timestamp:""},
    {id:4,sequence:4,event_type:"tool_call",tool_call_id:"tasks",tool_name:"update_plan",content:JSON.stringify({plan:[{step:"Inspect the tree",status:"completed"},{step:"Check queue delivery",status:"in_progress"},{step:"Review screenshots",status:"pending"}]}),timestamp:""},
    {id:5,sequence:5,event_type:"tool_result",tool_call_id:"tasks",tool_name:"update_plan",content:"Plan updated",timestamp:""},
  ];
  let extraFrames = "";
  let releaseSlow!:()=>void;
  const slow = new Promise<void>(resolve=>releaseSlow=resolve);
  await page.route("**/api/**",async route=>{
    const req=route.request(), url=new URL(req.url()),path=url.pathname;
    if(path==="/api/control/message") {
      const body=req.postDataJSON();posts.push(body);
      if(typeof reject === "string") return route.fulfill({status:400,contentType:"text/plain",body:reject});
      if(reject) return route.fulfill({json:{id:body.client_message_id,queued:false,message_accepted:false}});
      const id=body.client_message_id;
      const suffix=body.attachments?.length ? `\n\n<!-- paloma:attachment:${id} -->\nAttached context: read \`.paloma/messages/${id}/.paloma/attach.md\` (paths in that manifest are relative to \`.paloma/messages/${id}\`).` : "";
      const row={id,content:body.content+suffix,mission_id:"active"};
      if(!pending.some(item=>item.id===id)) pending.push(row);
      extraFrames+=`event: user_message\ndata: ${JSON.stringify({...row,queued:true})}\n\n`;
      if(loseReply){loseReply=false;return route.abort("failed");}
      return route.fulfill({json:{id:row.id,queued:true,message_accepted:true}});
    }
    if(path==="/api/control/queue") return route.fulfill({json:pending});
    if(path==="/api/control/stream") return route.fulfill({contentType:"text/event-stream",body:`event: text_delta\ndata: ${JSON.stringify({content:"Review in progress",sequence:6})}\n\n${extraFrames}`});
    if(path.endsWith("/events")) return route.fulfill({json:events});
    if(path==="/api/control/missions/active") return route.fulfill({json:mission});
    if(path==="/api/projects/test/files") {
      const dir=url.searchParams.get("path")??"";
      if(dir==="slow") await slow;
      const entries = !mixed ? [] : dir==="" ? [{name:"src",kind:"dir"},{name:"empty-dir",kind:"dir"},{name:"slow",kind:"dir"},{name:"README.md",kind:"file"}]
        :dir==="src"?[{name:"features",kind:"dir"},{name:"index.ts",kind:"file"}]
        :dir==="src/features"?[{name:"orb",kind:"dir"}]
        :dir==="src/features/orb"?[{name:"panels",kind:"dir"}]
        :dir==="src/features/orb/panels"?[{name:"deep.md",kind:"file"}]:[];
      return route.fulfill({json:{entries}});
    }
    const json = path==="/api/projects"?{projects:[{slug:"test",title:"test"},{slug:"empty",title:"Empty project"}]}
      :path==="/api/control/missions"?(url.searchParams.get("project")==="test"?[...(mixed?[mission]:[]),...done]:[])
      :path.endsWith("/files")?{entries:[]}:path.endsWith("/file")?{content:"# A verified project file\nSelected at depth five."}
      :path.endsWith("/controller")?{job:null,runs:[]}:path.endsWith("/crons")?{jobs:[]}:[];
    return route.fulfill({json});
  });
  await page.goto("/"); await page.getByRole("button",{name:"test",exact:true}).click();
  return {posts,pending,events,releaseSlow,loseNextReply:()=>loseReply=true,setReject:(value:boolean|string)=>reject=value,setStatus:(status:string)=>mission.status=status,
    deliver:(index=0)=>{
      const row=pending.splice(index,1)[0];
      events.push({id:++sequence,event_id:row.id,sequence,event_type:"user_message",content:row.content,timestamp:"",metadata:{queued:false}});
      extraFrames+=`event: user_message\ndata: ${JSON.stringify({...row,queued:false,sequence})}\n\n`;
      // A stale queued replay after delivery must never resurrect it.
      extraFrames+=`event: user_message\ndata: ${JSON.stringify({...row,queued:true,sequence:sequence-1})}\n\n`;
    }};
}

test("six Finished children terminate rails in both themes",async({page})=>{
  await setup(page);await page.getByRole("button",{name:"6 finished",exact:true}).click();
  const finished=page.locator('[data-tree-id="finished:test"]');
  await expect(finished.locator('.tree-junction')).toHaveClass(/last/);
  await expect(finished.locator('.tree-child-link')).toHaveCount(1);
  const last=page.locator('[data-tree-id="m:done-5"]');
  await expect(last.locator('.tree-rail')).toHaveCount(0);
  await expect(last.locator('.tree-junction')).toHaveClass(/last/);
  const geometry=await last.evaluate(el=>{const row=el.getBoundingClientRect(),rail=el.querySelector('.tree-junction')!.getBoundingClientRect();return{height:row.height,end:rail.bottom-row.top};});
  expect(geometry).toEqual({height:30,end:16});
  for(const theme of ["dark","light"]){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await page.locator('#orb-sidebar').screenshot({path:`test-results/orb-tree-six-${theme}.png`});}
});

test("mixed tree supports five levels, keyboard, selection, lazy loading and empty states",async({page})=>{
  const state=await setup(page,true);await page.getByRole("button",{name:"6 finished",exact:true}).click();
  for(const name of ["src","features","orb","panels"]) await page.getByRole("button",{name,exact:true}).click();
  const deep=page.getByRole("button",{name:"deep.md",exact:true});await deep.click();
  const entry=page.locator('[data-tree-id="pf:test:src/features/orb/panels/deep.md"]');
  await expect(entry).toHaveAttribute("aria-level","6");await expect(entry).toHaveAttribute("aria-selected","true");
  await expect(entry.locator('.tree-rail')).toHaveCount(2);
  await deep.focus();await page.keyboard.press("ArrowLeft");await expect(page.getByRole("button",{name:"panels",exact:true})).toBeFocused();
  await page.keyboard.press("ArrowLeft");await expect(deep).toHaveCount(0);
  await page.keyboard.press("ArrowRight");await expect(deep).toBeVisible();
  await page.getByRole("button",{name:"empty-dir",exact:true}).click();await expect(page.getByText("Empty folder",{exact:true})).toHaveCount(1);
  await page.getByRole("button",{name:"slow",exact:true}).click();await expect(page.getByText("Loading files…",{exact:true})).toBeVisible();
  await page.locator('#orb-sidebar').screenshot({path:"test-results/orb-tree-loading-dark.png"});state.releaseSlow();
  await expect(page.getByText("Empty folder",{exact:true})).toHaveCount(2);
  await page.getByRole("button",{name:"Empty project",exact:true}).click();await expect(page.getByText("No missions or files yet.")).toBeVisible();
  await page.getByRole("button",{name:"empty-dir",exact:true}).focus();await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("button",{name:"slow",exact:true})).toBeFocused();
  await page.keyboard.press("End");await expect(page.getByRole("button",{name:"Empty project",exact:true})).toBeFocused();
  for(const theme of ["dark","light"]){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await page.screenshot({path:`test-results/orb-tree-deep-${theme}.png`});}
  await page.getByRole("button",{name:"src",exact:true}).click();await expect(deep).toHaveCount(0);
  await page.locator('#orb-sidebar').screenshot({path:"test-results/orb-tree-collapsed-selected.png"});
});

test("combined queue and checklist survives failure, identical sends, reload and stale replay",async({page})=>{
  const state=await setup(page,true);state.releaseSlow();await page.getByRole("button",{name:"Queue and task review",exact:true}).click();
  const field=page.getByPlaceholder("Send follow-up");await expect(field).toBeVisible();
  await expect(page.getByRole("region",{name:"Tasks",exact:true})).toBeVisible();
  await expect(page.locator('.mission-tasks')).toContainText("1/3 completed");
  await expect(page.locator('.st-work-body')).toHaveCount(0);
  await expect(page.locator('.st-work-head')).toContainText("Read 1 file · 1 other tool");
  await page.locator('.st-work-head').click();await expect(page.locator('.st-tool')).toHaveCount(2);await page.locator('.st-work-head').click();
  await page.getByRole("button",{name:"Tasks",exact:true}).click();await expect(page.locator('.mission-tasks')).toBeFocused();
  await expect(page.getByRole("button",{name:"Plan",exact:true})).toHaveCount(0);
  await field.fill("@README");await page.getByRole("option",{name:"README.md",exact:true}).click();
  state.setReject(true);await field.fill("retry this @README.md");await field.press("Escape");await field.press("Enter");
  await expect(page.getByRole("alert")).toContainText("not accepted");await expect(field).toHaveValue("retry this @README.md");await expect(page.locator('.queued-messages')).toHaveCount(0);
  await expect(field).toHaveValue(/@README\.md/);
  state.setReject(false);
  for(let i=0;i<2;i++){await field.fill("same text @README.md");await field.press("Escape");await field.press("Enter");await expect(field).toHaveValue("");}
  await expect(page.locator('.queued-messages li')).toHaveCount(2);
  expect(state.posts[1].attachments).toEqual([{kind:"file",path:"README.md"}]);
  await expect(page.locator('.queued-messages')).toContainText("Attached context");
  await expect(page.locator('.queued-messages')).not.toContainText(".paloma");
  expect(state.posts.slice(-2).map(p=>p.client_message_id)[0]).not.toBe(state.posts.at(-1)!.client_message_id);
  await expect(page.locator('.st-text.live')).toHaveCount(1);
  await page.screenshot({path:"test-results/orb-queue-tasks-dark.png"});
  // The queue is restored from the API, with no browser-only queue cache.
  await page.reload();await page.getByRole("button",{name:"test",exact:true}).click();await page.getByRole("button",{name:"Queue and task review",exact:true}).click();
  await expect(page.locator('.queued-messages li')).toHaveCount(2);
  state.setStatus("completed"); // Status alone never confirms delivery.
  await page.waitForTimeout(2200);await expect(page.locator('.queued-messages li')).toHaveCount(2);
  state.deliver();await expect(page.locator('.queued-messages li')).toHaveCount(1,{timeout:8000});
  await expect(page.locator('.scroll .user').filter({hasText:"same text"})).toHaveCount(1);
  state.deliver();await expect(page.locator('.queued-messages')).toHaveCount(0,{timeout:8000});
  await expect(page.locator('.scroll .user').filter({hasText:"same text"})).toHaveCount(2);
  await page.evaluate(()=>document.documentElement.dataset.theme="light");await page.screenshot({path:"test-results/orb-queue-tasks-light.png"});
});


test("retry after a lost HTTP receipt reuses the accepted message ID",async({page})=>{
  const state=await setup(page,true);state.releaseSlow();
  await page.getByRole("button",{name:"Queue and task review",exact:true}).click();
  const field=page.getByPlaceholder("Send follow-up");await expect(field).toBeVisible();
  state.loseNextReply();await field.fill("accepted but reply lost");await field.press("Enter");
  await expect(page.getByRole("alert")).toBeVisible();await expect(field).toHaveValue("accepted but reply lost");
  await field.press("Enter");await expect(field).toHaveValue("");
  await expect.poll(() => state.posts.length).toBe(2);expect(state.posts[0].client_message_id).toBe(state.posts[1].client_message_id);
  expect(state.pending).toHaveLength(1);await expect(page.locator('.queued-messages li')).toHaveCount(1);
});


test("reserved attachment reference rejection keeps the follow-up draft and attachments",async({page})=>{
  const state=await setup(page,true);state.releaseSlow();
  await page.getByRole("button",{name:"Queue and task review",exact:true}).click();
  const field=page.getByPlaceholder("Send follow-up");
  await field.fill("@README");await page.getByRole("option",{name:"README.md",exact:true}).click();
  state.setReject("Message contains a reserved attachment reference. Remove it and use the attachment picker to attach context.");
  const prose="@README.md Quoted prose: "+"<!-- paloma:"+"attachment:malformed";
  await field.fill(prose);await field.press("Enter");
  await expect(page.getByRole("alert")).toContainText("reserved attachment reference");
  await expect(field).toHaveValue(prose);await expect(field).toHaveValue(/@README\.md/);
  await expect(page.locator('.queued-messages')).toHaveCount(0);
});
