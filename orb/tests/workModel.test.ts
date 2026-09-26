import { describe, it, expect } from "vitest";
import { latestChecklist, parseChecklist, workSummary } from "../src/workModel";
import { buildTranscript } from "../src/transcriptModel";
import fixtures from "./fixtures/task-tools.json";
import importer from "./fixtures/importer-events.json";
import { storedToStream, type StreamEvent } from "../src/stream";
const tool = (name:string,args:unknown,id=name):StreamEvent => ({type:"tool_call",data:{tool_call_id:id,name,args}});
describe("harness checklist adapters",()=>{
  it.each(fixtures)("accepts the source shape for $harness",fixture=>{
    const tasks=parseChecklist(fixture.name,fixture.args);
    expect(tasks).not.toBeNull();expect(tasks!.length).toBeGreaterThan(0);
    expect(parseChecklist(fixture.name,JSON.stringify(fixture.args))).toEqual(tasks);
  });
  it("ignores malformed payloads and unrelated tools without inventing pending tasks",()=>{
    for(const input of [null,{},"{broken",{todos:[null]},{todos:[{content:"hi",status:"whatever"}]},{todos:[{content:42,status:"pending"}]},{todos:[{content:"",status:"pending"}]}]) expect(parseChecklist("TodoWrite",input)).toBeNull();
    expect(parseChecklist("write",{todos:[{content:"hi",status:"pending"}]})).toBeNull();
    expect(parseChecklist("update_plan",{plan:[{step:"hi",status:"cancelled"}]})).toBeNull();
    expect(parseChecklist("todowrite",{todos:[{content:"hi",status:"cancelled"}]})).toEqual([{text:"hi",status:"cancelled"}]);
  });
  it("uses the latest valid checklist, retains raw events, and accepts explicit empty lists",()=>{
    const events=[tool("TodoWrite",fixtures[0].args,"a"),tool("update_plan",fixtures[1].args,"b"),tool("todowrite",{todos:"broken"},"c")];
    const items=buildTranscript(events);
    expect(latestChecklist(items)?.key).toBe("tool:b"); expect(items).toHaveLength(3);
    expect(latestChecklist(buildTranscript([...events,tool("TodoWrite",{todos:[]},"d")]))?.tasks).toEqual([]);
  });
  it("late tool arguments after a result still produce the real checklist",()=>{
    const items=buildTranscript([{type:"tool_result",data:{tool_call_id:"a",name:"TodoWrite",result:"ok"}},tool("TodoWrite",fixtures[0].args,"a")]);
    expect(latestChecklist(items)?.tasks).toHaveLength(2);expect(items).toMatchObject([{kind:"tool",done:true,result:"ok"}]);
  });
});
describe("factual work summaries",()=>{
  it("counts unique known file targets and actual harness command names",()=>{
    const items=buildTranscript([tool("Read",{file_path:"a.ts"},"a"),tool("read",{filePath:"a.ts"},"b"),tool("read_file",{path:"b.ts"},"c"),tool("functions.exec_command",{cmd:"pwd"}),tool("Bash",{command:"ls"}),tool("run_terminal_command",{command:"pwd"}),tool("Grep",{pattern:"hello"}),tool("Edit",{file_path:"a.ts"}),tool("update_plan",fixtures[1].args)]);
    expect(workSummary(items)).toBe("Read 2 files · 1 search · 3 commands · Edited 1 file · 1 other tool");
  });
  it("does not call unknown targets files, or shell searches file reads",()=>{
    expect(workSummary(buildTranscript([tool("read",{path:"a"},"a"),tool("read",{},"b"),tool("apply_patch","*** Begin Patch"),tool("bash",{command:"rg todo ."})]))).toBe("2 reads · 1 command · 1 edit");
    expect(workSummary(buildTranscript([tool("mcp__unknown__thing",{})]))).toBe("1 other tool");
  });
  it("accepts tool names from the captured production transcript",()=>{
    const events=importer.map(row=>storedToStream(row)).filter((e):e is StreamEvent=>!!e);
    const summary=workSummary(buildTranscript(events));
    expect(summary).toContain("commands");expect(summary).toContain("searches");expect(summary).toContain("other tools");
  });
});

import nativePlan from "./fixtures/codex-plan-notification.json";
it("reads the same native Codex normalization fixture exercised by the Rust translator",()=>{
  expect(parseChecklist("update_plan",nativePlan.normalized)).toEqual([{text:"Inspect implementation",status:"in_progress"},{text:"Run tests",status:"pending"}]);
});
