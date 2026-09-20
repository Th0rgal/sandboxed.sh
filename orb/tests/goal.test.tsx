import { describe, it, expect } from "vitest";
import { render } from "@solidjs/testing-library";
import { goalDraft, goalObjective, goalPrompt, missionTitle, displayTitle, EMPTY_GOAL_ERROR } from "../src/goal";
import { UserTurn } from "../src/Transcript";
import { LaunchStatus, missionGoal, type LaunchReceipt } from "../src/missionLaunch";
import type { Mission } from "../src/api";

const mission = (extra: Partial<Mission> = {}): Mission => ({ id: "m", title: null, status: "pending", history: [], created_at: "", updated_at: "", ...extra });

describe("goal draft parsing mirrors the server", () => {
  it.each([
    ["/goal write the docs", "write the docs"],
    ["   /goal   spaced   ", "spaced"],
    ["/goal\nMulti-line objective\nwith details", "Multi-line objective\nwith details"],
    ["/goal /goal nested", "/goal nested"],
  ])("%j is a goal", (text, objective) => {
    expect(goalDraft(text)).toEqual({ kind: "goal", objective });
    expect(goalObjective(text)).toBe(objective);
  });
  it.each(["/goal", "/goal   ", "/goal\n"])("%j needs an objective", (text) => {
    expect(goalDraft(text)).toEqual({ kind: "empty" });
    expect(goalObjective(text)).toBeNull();
  });
  it("names the empty-goal composer error so a valid edit can clear only that alert", () => {
    expect(EMPTY_GOAL_ERROR).toContain("Add an objective after /goal");
    expect(goalDraft("/goal Ship it").kind).toBe("goal");
  });
  it.each(["/goals are nice", "plain message", "please run /goal literally", "", "goal: x"])("%j is not a goal", (text) => {
    expect(goalDraft(text)).toEqual({ kind: "none" });
  });
  it("produces the canonical prompt the backend goal drivers expect", () => {
    expect(goalPrompt(goalObjective("/goal\n  Ship it  ")!)).toBe("/goal Ship it");
  });
});

describe("mission titles", () => {
  it("uses the objective, never the raw /goal command", () => {
    expect(missionTitle("/goal Ship the release notes")).toBe("Ship the release notes");
    expect(missionTitle("/goal\nFirst line of the objective\nsecond line")).toBe("First line of the objective");
  });
  it("keeps the exact objective in the prompt while shortening only the title", () => {
    const objective = "Check remote startup without losing this draft";
    expect(missionTitle(`/goal ${objective}`)).toBe("Check remote startup without losing this…");
    expect(goalPrompt(goalObjective(`/goal ${objective}`)!)).toBe(`/goal ${objective}`);
  });
  it("still titles plain prompts by their first line", () => {
    expect(missionTitle("Fix the flaky test\nand explain why")).toBe("Fix the flaky test");
    expect(missionTitle("x".repeat(60))).toBe(`${"x".repeat(41)}…`);
  });
  it("shows stored raw /goal titles from older clients as their objective", () => {
    expect(displayTitle("/goal Original saved objective")).toBe("Original saved objective");
    expect(displayTitle("Remote task")).toBe("Remote task");
    expect(displayTitle(null)).toBeNull();
  });
});

describe("goal indicators", () => {
  it("renders a /goal user turn as a Goal turn with the exact objective", () => {
    const { container } = render(() => <UserTurn text="/goal Check the guard" />);
    const turn = container.querySelector(".user")!;
    expect(turn.classList.contains("goal")).toBe(true);
    expect(turn.querySelector(".goal-tag")?.textContent).toBe("Goal");
    expect(turn.querySelector(":scope > span:last-child")?.textContent).toBe("Check the guard");
  });
  it("leaves ordinary turns untouched", () => {
    const { container } = render(() => <UserTurn text="Hello /goal not a command" />);
    expect(container.querySelector(".goal-tag")).toBeNull();
    expect(container.querySelector(".user")?.textContent).toBe("Hello /goal not a command");
  });
  it("derives the goal from persisted state first, then the accepted prompt, then history", () => {
    const receipt: LaunchReceipt = { prompt: "/goal From receipt", nodeId: "core", destination: "Core" };
    expect(missionGoal(mission({ goal_mode: true, goal_objective: "Stored" }), receipt)).toBe("Stored");
    expect(missionGoal(mission(), receipt)).toBe("From receipt");
    expect(missionGoal(mission({ history: [{ role: "user", content: "/goal From history" }] }))).toBe("From history");
    expect(missionGoal(mission({ goal_mode: false, goal_objective: "ignored" }))).toBeNull();
    expect(missionGoal(null)).toBeNull();
  });
  it("marks the launch status as a goal without changing the phase text", () => {
    const { container } = render(() => <LaunchStatus destination="DGX Spark" submitting goal="Stored" />);
    expect(container.querySelector(".goal-tag")?.textContent).toBe("Goal");
    expect(container.querySelector("[role=status]")?.textContent).toContain("Starting on DGX Spark");
  });
});
