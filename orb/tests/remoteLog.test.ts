import { describe, it, expect } from "vitest";
import { remoteLog } from "../src/remoteLog";
const envelope = "Remote node 'dgx-spark' job 3dff58d2-508c-458e-90c1-701e402a6b5f finished with state 'succeeded' (exit Some(0))\n\nlog tail:\n";
const text = JSON.stringify({type:"text",sessionID:"ses_native",part:{id:"part_1",text:"## Status\n\nRunning and durable."}});
describe("legacy OpenCode remote log", () => {
  it("decodes text after a truncated line and retains the exact log", () => {
    const raw=envelope+'truncated JSON...\n'+text+'\n'+text+'\n'+JSON.stringify({type:"step_finish",sessionID:"ses_native",part:{tokens:{total:38487}}});
    expect(remoteLog(raw)).toEqual({text:"## Status\n\nRunning and durable.",details:raw});
  });
  it("does not reinterpret ordinary JSON, malformed logs, or other harnesses", () => {
    for(const raw of [text,envelope+'broken',envelope+JSON.stringify({type:"text",data:"hello"})]) expect(remoteLog(raw)).toEqual({text:raw});
  });
  it("retains failure status even when the log contains a text part", () => {
    const raw=envelope.replace("'succeeded'", "'failed'")+text;
    expect(remoteLog(raw).text).toContain("'failed'");
    expect(remoteLog(raw).text).toContain("## Status\n");
  });
});
