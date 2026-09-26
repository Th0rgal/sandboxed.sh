import { createSignal } from "solid-js";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { it, expect, vi, afterEach } from "vitest";
import * as api from "../src/api";
import { SteerComposer } from "../src/SteerComposer";
import { CronGlyph, cronState, buildEntries } from "../src/Controller";
import { getProjectCronFromJob } from "../src/cronSchema";
import fixtures from "./fixtures/hermes-jobs.json";

afterEach(() => vi.restoreAllMocks());
const inbox: api.ProjectSteers = { pending: [{ id: "steer-1", body: "Check the report", created_at: "2026-09-21", origin: "orb" }], recent: [] };

it("keeps the saved steer when triggering fails and reports the separate failure", async () => {
  const save = vi.spyOn(api, "addProjectSteer").mockResolvedValue(inbox);
  vi.spyOn(api, "controllerAction").mockRejectedValue(new Error("Scheduler unavailable"));
  const [steers, setSteers] = createSignal<api.ProjectSteers>({ pending: [], recent: [] });
  const ui = render(() => <SteerComposer slug="test" steers={steers()} running={false} onSteers={setSteers} />);
  fireEvent.input(ui.getByRole("textbox"), { target: { value: "Check the report" } });
  fireEvent.click(ui.getByRole("button", { name: "Steer", exact: true }));
  await waitFor(() => expect(ui.container.textContent).toContain("Steer saved, but the controller could not start"));
  expect(ui.container.textContent).not.toContain("Run requested");
  expect(save).toHaveBeenCalledTimes(1);
  expect(steers().pending).toHaveLength(1);
});

it("does not trigger another run while a controller is already running", async () => {
  vi.spyOn(api, "addProjectSteer").mockResolvedValue(inbox);
  const run = vi.spyOn(api, "controllerAction");
  const [steers, setSteers] = createSignal<api.ProjectSteers>({ pending: [], recent: [] });
  const ui = render(() => <SteerComposer slug="test" steers={steers()} running onSteers={setSteers} />);
  fireEvent.input(ui.getByRole("textbox"), { target: { value: "Check the report" } });
  fireEvent.click(ui.getByRole("button", { name: "Steer", exact: true }));
  await waitFor(() => expect(ui.container.textContent).toContain("Awaiting pickup"));
  expect(run).not.toHaveBeenCalled();
});

it("keeps the clock identity when a cron is paused", () => {
  const job = getProjectCronFromJob("test", fixtures.hourly).job!;
  const [paused, setPaused] = createSignal(true);
  const ui = render(() => <CronGlyph job={{ ...job, enabled: !paused(), state: paused() ? "paused" : "scheduled" }} />);
  expect(ui.container.querySelector(".cron-glyph.paused .cron-clock-hands")).not.toBeNull();
  setPaused(false);
  expect(ui.container.querySelector(".cron-clock-hands")).not.toBeNull();
  expect(ui.container.querySelector(".cron-glyph.paused")).toBeNull();
});

it("keeps a paused schedule visible while an already-started run finishes", () => {
  const job = getProjectCronFromJob("test", fixtures.hourly).job!;
  expect(cronState({ ...job, enabled: false, state: "paused" }, true)).toBe("paused");
  expect(cronState({ ...job, enabled: false, state: "running" }, true)).toBe("paused");
  expect(cronState({ ...job, enabled: true, state: "scheduled" }, true)).toBe("running");
});

it("moves a consumed instruction out of the composer and into the dated history", () => {
  const steer = { ...inbox.pending[0], consumed_at: "2026-09-21T15:43:37Z" };
  const ui = render(() => <SteerComposer slug="test" steers={{ pending: [], recent: [steer] }} running={false} onSteers={() => {}} />);
  expect(ui.container.textContent).not.toContain(steer.body);
  const entries = buildEntries([], [steer, inbox.pending[0]]);
  expect(entries.map(e => e.kind)).toEqual(["day", "steer"]);
  expect(entries[1]).toMatchObject({ kind: "steer", steer });
});
