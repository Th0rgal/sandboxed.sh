import { test,expect } from "@playwright/test";

test("actual App project chooser searches, selects and creates with keyboard and errors",async({page})=>{
 const errors:string[]=[];page.on("pageerror",e=>errors.push(e.message));
 let fail=true;const writes:any[]=[];
 const projects=Array.from({length:25},(_,i)=>({slug:`project-${i}`,title:i===0?"Verity":`Project ${i}`,status:"active",updated_at:"2026-09-20"}));
 await page.addInitScript(()=>{localStorage.setItem("orb.apiUrl",location.origin);localStorage.setItem("orb.jwt","test");localStorage.setItem("orb-theme","dark");});
 await page.route("**/api/**",async route=>{
  const r=route.request(),path=new URL(r.url()).pathname;
  if(path==="/api/projects"&&r.method()==="PUT"){
   writes.push(r.postDataJSON());if(fail)return route.fulfill({status:503,body:"Project service unavailable"});
   const item={...r.postDataJSON(),status:"active",updated_at:"2026-09-20"};projects.push(item);return route.fulfill({json:item});
  }
  const json=path==="/api/control/queue"?[]:path==="/api/projects"?{projects}:path==="/api/control/missions"&&new URL(r.url()).searchParams.get("project")==="project-0"?Array.from({length:4},(_,i)=>({id:`mission-${i}`,title:["Importer slice: modifiers + structs","Address allocation bounds","Review the guard","Field generation"][i],status:"active",workspace_name:"project-0"})):path.endsWith("/missions")?[]:path.endsWith("/files")?{entries:[]}:path.endsWith("/crons")?{jobs:[]}:path.endsWith("/controller")?{job:null,runs:[]}:{};
  await route.fulfill({json});
 });
 await page.goto("/");
 await page.getByRole("button",{name:"Verity",exact:true}).click();
 const agent=page.getByRole("button",{name:/Importer slice/});await expect(agent).toBeVisible();
  expect((await agent.boundingBox())!.height).toBe(30);
  expect((await page.getByRole("button",{name:"Verity",exact:true}).boundingBox())!.height).toBe(30);
 await expect(agent).not.toContainText("project-0");
 await page.locator("#orb-sidebar").screenshot({path:"test-results/orb-sidebar-compact.png"});
 const trigger=page.getByRole("button",{name:"Choose project",exact:true});await trigger.click();
 const search=page.getByRole("combobox",{name:"Search projects"});await expect(search).toBeFocused();
 await expect(search).toHaveCSS("outline-style","none");
 await expect(search).toHaveCSS("border-radius","0px");
 await expect(page.getByRole("option").first()).toContainText("Default");
 await expect(page.getByRole("option",{name:"Default Current project"})).toHaveAttribute("aria-selected","true");
 expect(await page.locator(".project-options").evaluate(el=>el.scrollHeight>el.clientHeight)).toBe(true);
 await page.screenshot({path:"test-results/orb-project-picker.png"});
 await page.evaluate(()=>document.documentElement.dataset.theme="light");
 await page.screenshot({path:"test-results/orb-project-picker-light.png"});
 await page.evaluate(()=>document.documentElement.dataset.theme="dark");
 await search.fill("Project 24");await expect(page.getByRole("option")).toHaveCount(1);await search.press("Enter");await expect(trigger).toContainText("Project 24");await expect(trigger).toBeFocused();
 await trigger.click();await search.fill("no such project");await expect(page.getByText("No matching projects")).toBeVisible();await search.press("Escape");await expect(trigger).toBeFocused();
 await trigger.click();await page.locator(".titlebar").click({position:{x:500,y:15}});await expect(page.getByRole("dialog",{name:"Choose project"})).toHaveCount(0);
 await trigger.click();await search.press("ArrowDown");await search.press("Enter");await expect(trigger).toContainText("Verity");
 await trigger.click();await page.getByRole("button",{name:"New project…",exact:true}).click();
 const dialog=page.getByRole("dialog",{name:"New project",exact:true});await expect(dialog).toBeVisible();await expect(page.getByLabel("Project name",{exact:true})).toBeFocused();
 await expect(page.getByRole("button",{name:"Create project",exact:true})).toBeDisabled();
 await page.getByLabel("Project name",{exact:true}).fill("Project 1");await page.getByRole("button",{name:"Create project",exact:true}).click();await expect(page.getByRole("alert")).toContainText("already exists");expect(writes).toHaveLength(0);
 await page.getByLabel("Project name",{exact:true}).fill("Fresh notes");await page.getByRole("button",{name:"Create project",exact:true}).click();await expect(page.getByRole("alert")).toContainText("Project service unavailable");
 await page.screenshot({path:"test-results/orb-project-create.png"});
 fail=false;await page.getByRole("button",{name:"Create project",exact:true}).click();await expect(dialog).toHaveCount(0);await expect(trigger).toContainText("Fresh notes");await expect(trigger).toBeFocused();
 expect(writes.at(-1)).toEqual({slug:"fresh-notes",title:"Fresh notes"});
 await trigger.click();await page.getByRole("button",{name:"New project…",exact:true}).click();await page.getByRole("button",{name:"Close",exact:true}).click();await expect(trigger).toBeFocused();
 expect(errors).toEqual([]);
});

test("Default is created on first use; failed creation retains the draft and retries do not duplicate it", async ({page}) => {
  let fail = true;
  const projects: {slug:string;title:string}[] = [];
  let projectWrites = 0;
  const missions: {project:string}[] = [];
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin); localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb.harnessPick", JSON.stringify({backend:"grok",model:"grok-test"}));
  });
  await page.route("**/api/**", async route => {
    const request = route.request(), path = new URL(request.url()).pathname;
    if (path === "/api/projects" && request.method() === "PUT") {
      projectWrites++;
      if (fail) return route.fulfill({status:503,body:"Cannot create project"});
      projects.push(request.postDataJSON());
      return route.fulfill({json:projects[0]});
    }
    if (path === "/api/control/missions" && request.method() === "POST") {
      missions.push(request.postDataJSON());
      return route.fulfill({status:503,body:"Runner unavailable"});
    }
    const json = path === "/api/projects" ? {projects}
      : path === "/api/backends" ? [{id:"grok",name:"Grok"}]
      : path === "/api/providers/backend-models" ? {backends:{grok:[{value:"grok-test",label:"Grok test"}]}}
      : path.endsWith("/missions") || path.endsWith("/queue") ? []
      : path.endsWith("/files") ? {entries:[]}
      : path.endsWith("/crons") ? {jobs:[]} : {};
    await route.fulfill({json});
  });
  await page.goto("/");
  await expect(page.getByRole("button", {name:"Choose project",exact:true})).toContainText("Default");
  await expect(page.getByRole("button", {name:"Grok",exact:true})).toBeVisible();
  expect(projectWrites).toBe(0);
  const input = page.getByPlaceholder("Describe a task, / for commands, @ for context");
  await input.fill("A random idea"); await input.press("Enter");
  await expect(page.getByRole("alert")).toContainText("Cannot create project");
  await expect(input).toHaveValue("A random idea"); expect(missions).toHaveLength(0);
  fail = false; await input.press("Enter");
  await expect(page.getByRole("alert")).toContainText("Runner unavailable");
  expect(projects).toEqual([{slug:"default",title:"Default"}]);
  expect(missions[0].project).toBe("default");
  await input.press("Enter"); await expect.poll(() => missions.length).toBe(2);
  expect(projectWrites).toBe(2);
  await page.reload();
  await page.getByRole("button",{name:"Choose project",exact:true}).click();
  await expect(page.getByRole("option",{name:"Default Current project"})).toHaveCount(1);
  expect(projectWrites).toBe(2);
});
