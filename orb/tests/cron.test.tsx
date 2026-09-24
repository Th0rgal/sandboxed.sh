import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@solidjs/testing-library";
import fixtures from "./fixtures/hermes-jobs.json";
import { getProjectCronFromJob, hermesPatch, ignoredCronFields, normalizeControllerView, scheduleExpression } from "../src/cronSchema";
import { CronForm, draftOf } from "../src/ControllerSettings";
import { parseSchedule, describeSchedule, SchedulePicker } from "../src/SchedulePicker";
import { getProjectCron, getProjectController, listProjectCrons, projectCronAction, updateProjectCron } from "../src/api";
import { PopupMenu } from "../src/Menu";

const view = () => getProjectCronFromJob("notes", fixtures.hourly);
const response = (body: unknown) => new Response(JSON.stringify(body), { status: 200 });

describe("real Hermes records", () => {
  it("maps object schedules, repeat counters and every execution setting", () => {
    const result = getProjectCronFromJob("notes", fixtures.updated);
    expect(result.job?.schedule).toBe("every 1h");
    expect(result.settings).toMatchObject({ repeat_times: 12, repeat_completed: 3, model: "gpt-5", provider: "openai", reasoning_effort: "high", workdir: fixtures.updated.workdir, continuity: true, failure_deliver: "local", skills: ["project-notes"] });
    expect(scheduleExpression(fixtures.weekdays.schedule)).toBe("0 9 * * 1-5");
    expect(scheduleExpression(fixtures.once.schedule)).toBe("2099-04-05T09:00:00+02:00");
    expect(getProjectCronFromJob("notes", fixtures.once).settings?.repeat_times).toBe(1);
    expect(getProjectCronFromJob("notes", fixtures.weekdays).settings?.repeat_times).toBeNull();
  });
  it("normalizes primary controller records before the form calls trim", async () => {
    for (const job of [fixtures.hourly, fixtures.weekdays, fixtures.once]) {
      const result = normalizeControllerView({ slug: "notes", job, runs: [] });
      expect(() => draftOf(result)).not.toThrow();
      expect(draftOf(result).schedule).toBe(scheduleExpression(job.schedule));
    }
    const fetcher = vi.spyOn(globalThis, "fetch").mockResolvedValue(response({ slug: "notes", job: fixtures.hourly, runs: [] }));
    try { expect((await getProjectController("notes")).job?.schedule).toBe("every 1h"); }
    finally { fetcher.mockRestore(); }
  });
  it("shows real saved defaults without turning snapshots into editable overrides", () => {
    const result = getProjectCronFromJob("notes", fixtures.snapshot);
    expect(result.settings).toMatchObject({ model: null, provider: null, model_snapshot: "fixture-local-model", provider_snapshot: "custom" });
    expect(draftOf(result)).toMatchObject({ model: "", provider: "", repeat: "" });
    render(() => <CronForm draftKey="snapshots" view={result} save={async () => result} onSaved={() => {}} />);
    expect(screen.getByText("Saved default: fixture-local-model")).toBeTruthy();
    expect(screen.getByText("Saved default: custom")).toBeTruthy();
    expect((screen.getByLabelText("Model") as HTMLInputElement).value).toBe("");
    expect(screen.queryByText("Save")).toBeNull();
  });
  it("maps continuity to the real self context reference", () => {
    expect(hermesPatch({ continuity: true })).toEqual({ context_from: ["self"] });
    expect(hermesPatch({ continuity: false }, fixtures.hourly)).toEqual({ context_from: [] });
  });
  it("uses the same mapping for reads, listing, edits and run actions", async () => {
    const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation(async (_url, init) => {
      if (init?.method === "POST") return response({ job: fixtures.run });
      if (init?.method === "PATCH") return response({ job: fixtures.updated });
      if (String(_url).endsWith("/crons")) return response({ jobs: [fixtures.hourly] });
      return response({ job: fixtures.hourly });
    });
    try {
      expect((await listProjectCrons("notes"))[0].schedule).toBe("every 1h");
      expect((await getProjectCron("notes", fixtures.hourly.id)).settings?.continuity).toBe(true);
      expect((await updateProjectCron("notes", fixtures.hourly.id, { repeat: 12 })).settings?.repeat_completed).toBe(3);
      const updated = JSON.parse(fetcher.mock.calls[2][1]!.body as string);
      expect(updated.repeat).toBe(12);
      const result = await projectCronAction("notes", fixtures.hourly.id, "run");
      expect(result.settings?.model).toBe("gpt-5");
      expect(fetcher.mock.calls.at(-1)?.[1]?.method).toBeUndefined();
    } finally { fetcher.mockRestore(); }
  });
});

describe("shared cron form", () => {
  it("preserves drafts and the original base across unmount; discard resets", async () => {
    const save = vi.fn(async () => view());
    render(() => <CronForm draftKey="test" view={view()} save={save} onSaved={() => {}} />);
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "Unfinished edit" } });
    cleanup();
    render(() => <CronForm draftKey="test" view={view()} save={save} onSaved={() => {}} />);
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Unfinished edit");
    fireEvent.click(screen.getByText("Discard"));
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe(fixtures.hourly.name);
    expect(save).not.toHaveBeenCalled();
  });
  it("defaults creation to the project route and requires an explicit local choice when missing", async () => {
    const save = vi.fn(async () => view());
    render(() => <CronForm creating draftKey="route" view={{ slug: "notes", job: view().job, runs: [] }} deliveryRoute={{ ready: false, loading: false, error: null }} save={save} onSaved={() => {}} />);
    expect((screen.getByLabelText("Delivery") as HTMLInputElement).value).toBe("project:notes");
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/No delivery route is bound yet/)).toBeTruthy();
    fireEvent.input(screen.getByLabelText("Delivery"), { target: { value: "local" } });
    fireEvent.input(screen.getByLabelText("Instruction"), { target: { value: "Read local notes" } });
    fireEvent.click(screen.getByText("Create"));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ deliver: "local" })));
  });
  it("validates name and repeat, then sends numeric repeat with skills on create", async () => {
    const save = vi.fn(async () => view());
    render(() => <CronForm creating draftKey="new" view={view()} save={save} onSaved={() => {}} />);
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "" } });
    fireEvent.click(screen.getByText("Create"));
    expect(screen.getByText("Name, instruction, and schedule are required.")).toBeTruthy();
    expect(save).not.toHaveBeenCalled();
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "New notes" } });
    fireEvent.input(screen.getByLabelText("Stops after"), { target: { value: "0" } });
    fireEvent.click(screen.getByText("Create"));
    expect(screen.getByText(/positive whole number/)).toBeTruthy();
    fireEvent.input(screen.getByLabelText("Stops after"), { target: { value: "5" } });
    fireEvent.input(screen.getByPlaceholderText("Add skill"), { target: { value: "review" } });
    fireEvent.click(screen.getByText("Create"));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ name: "New notes", repeat: 5, skills: ["project-notes", "review"], continuity: true, reasoning_effort: "high" })));
  });
  it("keeps rejected overrides visible when an older Hermes API filters them", async () => {
    render(() => <CronForm draftKey="filtered" view={view()} save={async () => view()} onSaved={() => {}} />);
    const model = screen.getAllByPlaceholderText("Hermes default")[0];
    fireEvent.input(model, { target: { value: "new-model" } });
    fireEvent.click(screen.getByText("Save"));
    await screen.findByText(/Hermes did not retain: model/);
    expect((model as HTMLInputElement).value).toBe("new-model");
    expect(ignoredCronFields({ model: "new-model" }, view())).toEqual(["model"]);
  });
  it("reveals a created cron once and carries dropped overrides into its edit draft", async () => {
    const saved = vi.fn();
    render(() => <CronForm creating draftKey="new-filtered" view={view()} save={async () => view()} onSaved={saved} />);
    fireEvent.input(screen.getByLabelText("Model"), { target: { value: "requested-model" } });
    fireEvent.click(screen.getByText("Create"));
    await waitFor(() => expect(saved).toHaveBeenCalledTimes(1));
    expect(saved.mock.calls[0][1]).toContain("Hermes did not retain: model");
    cleanup();
    render(() => <CronForm draftKey={`edit:notes:${fixtures.hourly.id}`} view={view()} save={async () => view()} onSaved={() => {}} />);
    expect((screen.getByLabelText("Model") as HTMLInputElement).value).toBe("requested-model");
  });
  it("retains drafts after a rejected save", async () => {
    render(() => <CronForm draftKey="failure" view={view()} save={async () => { throw new Error("Scheduler unavailable"); }} onSaved={() => {}} />);
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "Keep this" } });
    fireEvent.click(screen.getByText("Save"));
    await screen.findByText("Scheduler unavailable");
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Keep this");
  });
});

describe("schedule and menu interaction", () => {
  it("keeps duration-only and zoned timestamps out of the recurring/naive editors", () => {
    expect(parseSchedule("30m").mode).toBe("custom");
    expect(parseSchedule("2099-04-05T09:00:00+02:00").mode).toBe("custom");
    expect(parseSchedule("0 9 * * 9").mode).toBe("custom");
    expect(describeSchedule(parseSchedule("every 1h"))).toBe("Every hour");
    expect(describeSchedule(parseSchedule("0 9 * * 1-5"))).toBe("Weekdays at 09:00");
  });
  it("opens from one summary and returns focus on Escape", async () => {
    render(() => <SchedulePicker value="every 1h" onChange={() => {}} />);
    const trigger = screen.getByRole("button", { name: "Schedule" });
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(trigger);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Schedule type")));
    fireEvent.keyDown(screen.getByLabelText("Schedule type"), { key: "Escape" });
    expect(document.activeElement).toBe(trigger);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
  it("supports menu arrows and Escape focus restoration", () => {
    const trigger = document.createElement("button"); document.body.append(trigger); trigger.focus();
    const close = vi.fn();
    render(() => <PopupMenu x={20} y={20} onClose={close} items={[{ kind: "item", label: "Folder", onClick: () => {} }, { kind: "item", label: "Cron", onClick: () => {} }]} />);
    expect(document.activeElement?.textContent).toBe("Folder");
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("Cron");
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(close).toHaveBeenCalled(); expect(document.activeElement).toBe(trigger); trigger.remove();
  });
});

 it("keeps successful creation successful when storage cleanup throws", async () => {
   const saved = vi.fn();
   render(() => <CronForm creating draftKey="storage-failure" view={view()} save={async () => view()} onSaved={saved} />);
   const remove = vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => { throw new Error("unavailable"); });
   try { fireEvent.click(screen.getByText("Create")); await waitFor(() => expect(saved).toHaveBeenCalledOnce()); }
   finally { remove.mockRestore(); }
 });

it("shows prompt limit failures next to Save and retains the edited instruction",async()=>{
 const save=vi.fn();
 render(()=><CronForm draftKey="visible-limit" view={view()} save={save} onSaved={()=>{}}/>);
 const input=screen.getByLabelText('Instruction') as HTMLTextAreaElement;
 fireEvent.input(input,{target:{value:'x'.repeat(7466)}});
 fireEvent.click(screen.getByText('Save'));
 expect(save).not.toHaveBeenCalled();
 const error=screen.getByText(/Shorten it before saving/);
 expect(error.closest('.cs-save-area')).toBeTruthy();
 expect(input.value.length).toBe(7466);
 expect(screen.getByText('7,466 / 5,000 characters')).toBeTruthy();
});
