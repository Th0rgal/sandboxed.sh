import { render, cleanup } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, expect, it } from "vitest";
import { MissionGlyph, missionStatusPresentation } from "../src/MissionGlyph";

afterEach(cleanup);

it("keeps agent identity while a live mission completes, pauses or fails", () => {
  const [status, setStatus] = createSignal("active");
  const { container } = render(() => <MissionGlyph status={status()} />);
  const bot = container.querySelector(".mission-glyph > svg");
  expect(container.querySelector(".mission-status-spin")).not.toBeNull();
  setStatus("completed");
  expect(container.querySelector(".mission-glyph")?.getAttribute("title")).toBe("Completed");
  expect(container.querySelector(".mission-glyph > svg")).toBe(bot);
  expect(container.querySelector(".mission-status-spin")).toBeNull();
  for (const state of ["idle", "blocked", "paused", "failed", "interrupted", "unrecognized"]) {
    setStatus(state);
    expect(container.querySelector(".success")).toBeNull();
    expect(container.querySelector(".mission-glyph > svg")).toBe(bot);
    expect(container.querySelector(".mission-status-spin")).toBeNull();
  }
});

 it.each(['awaiting_user','waiting_user','acknowledged'])('renders %s as ready, without implying a question or success',status=>{
  const {container}=render(()=><MissionGlyph status={status}/>);
  expect(container.querySelector('.mission-glyph')?.getAttribute('title')).toBe('Ready for a follow-up');
  expect(container.querySelector('.mission-status-mark')).toBeNull();
 });
 it('only explicit pending interactions request attention',()=>{
  expect(missionStatusPresentation('active',{id:'q',method:'questions'}).label).toBe('Waiting for your reply');
  expect(missionStatusPresentation('active',{id:'p',method:'permission'}).label).toBe('Approval requested');
  expect(missionStatusPresentation('completed',{id:'old',method:'questions'}).label).toBe('Completed');
 });
