import { describe, it, expect } from "vitest";
import { render } from "@solidjs/testing-library";
import { Transcript, buildTranscript } from "../src/Transcript";
import type { StreamEvent } from "../src/stream";

const ev = (type: string, data: Record<string, unknown>): StreamEvent => ({ type, data });

describe("thinking fold", () => {
  it("shows one Thinking header and the thought text, not a nested work fold", () => {
    const items = buildTranscript([
      ev("thinking", { content: "Need to inspect the kernel.", done: false }),
    ]);
    const { container } = render(() => <Transcript items={items} />);
    const heads = container.querySelectorAll(".st-think-head");
    expect(heads).toHaveLength(1);
    expect(heads[0].textContent).toMatch(/^Thinking/);
    expect(container.querySelectorAll(".st-work")).toHaveLength(0);
    expect(container.querySelector(".st-think-body")?.textContent).toBe("Need to inspect the kernel.");
  });

  it("keeps tools in a work fold and inlines thought text without a second Thinking header", () => {
    const items = buildTranscript([
      ev("thinking", { content: "I'll read the file.", done: true }),
      ev("tool_call", { tool_call_id: "a", name: "read", args: { path: "guard.ts" } }),
    ]);
    const { container } = render(() => <Transcript items={items} />);
    expect(container.querySelectorAll(".st-work-head")).toHaveLength(1);
    expect(container.querySelectorAll(".st-think-head")).toHaveLength(0);
    container.querySelector<HTMLButtonElement>(".st-work-head")!.click();
    expect(container.querySelector(".st-think-body")?.textContent).toBe("I'll read the file.");
    expect(container.querySelector(".st-tool-name")?.textContent).toBe("read");
  });
});

it("latest checklist is visible outside folded raw work, with real progress and no Plan/Build control",()=>{
  const items=buildTranscript([
    ev("tool_call",{tool_call_id:"old",name:"TodoWrite",args:{todos:[{content:"Old task",status:"pending",activeForm:"Working"}]}}),
    ev("tool_call",{tool_call_id:"new",name:"update_plan",args:{plan:[{step:"Plan a build",status:"completed"},{step:"Check result",status:"in_progress"}]}}),
    ev("tool_call",{tool_call_id:"bad",name:"todowrite",args:{todos:[{content:"Invented",status:"maybe"}]}}),
  ]);
  const {container}=render(()=><Transcript items={items}/>);
  expect(container.querySelectorAll('.st-work-body')).toHaveLength(0);
  expect(container.querySelector('.mission-tasks')?.textContent).toContain('1/2 completed');
  expect(container.querySelector('.mission-tasks')?.textContent).not.toContain('Old task');
  expect(container.querySelector('.mission-tasks')?.textContent).not.toContain('Invented');
  expect(container.querySelector('progress')?.value).toBe(1);
  container.querySelector<HTMLButtonElement>('.st-work-head')!.click();
  expect(container.querySelectorAll('.st-tool')).toHaveLength(3);
  expect([...container.querySelectorAll('button')].some(button=>['Plan','Build'].includes(button.textContent??''))).toBe(false);
});
