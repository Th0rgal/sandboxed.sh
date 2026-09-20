import { describe, it, expect, vi } from "vitest";
import { createSignal, Show } from "solid-js";
import { fireEvent, render, screen } from "@solidjs/testing-library";
import { Dialog } from "../src/Dialog";
import { SchedulePicker } from "../src/SchedulePicker";
import { hasFocusScope } from "../src/focusScope";

function NestedSchedule() {
  const [open, setOpen] = createSignal(false);
  return <>
    <button onClick={() => setOpen(true)}>Open creation</button>
    <Show when={open()}><Dialog title="Create cron" onClose={() => setOpen(false)} footer={<button>Save cron</button>}>
      <input aria-label="Name" />
      <SchedulePicker value="every 1h" onChange={() => {}} />
      <button disabled>Disabled action</button>
      <button hidden>Hidden action</button>
    </Dialog></Show>
  </>;
}

describe("dialog focus ownership", () => {
  it("first Escape closes only the schedule; second closes its modal and restores the opener", () => {
    render(() => <NestedSchedule />);
    const opener = screen.getByText("Open creation"); opener.focus(); fireEvent.click(opener);
    expect(screen.getByRole("dialog", { name: "Create cron" }).getAttribute("aria-modal")).toBe("true");
    const trigger = screen.getByRole("button", { name: "Schedule" }); trigger.focus(); fireEvent.click(trigger);
    const globalEscape = vi.fn(); window.addEventListener("keydown", globalEscape);
    try {
      fireEvent.keyDown(screen.getByLabelText("Schedule type"), { key: "Escape" });
      expect(screen.queryByRole("dialog", { name: "Schedule editor" })).toBeNull();
      expect(screen.getByRole("dialog", { name: "Create cron" })).toBeTruthy();
      expect(document.activeElement).toBe(trigger);
      expect(globalEscape).not.toHaveBeenCalled();
      fireEvent.keyDown(trigger, { key: "Escape" });
      expect(screen.queryByRole("dialog", { name: "Create cron" })).toBeNull();
      expect(document.activeElement).toBe(opener);
      expect(globalEscape).not.toHaveBeenCalled();
      expect(hasFocusScope()).toBe(false);
    } finally { window.removeEventListener("keydown", globalEscape); }
  });
  it("traps Tab/Shift+Tab in the modal and independently in its schedule popover", () => {
    render(() => <NestedSchedule />); fireEvent.click(screen.getByText("Open creation"));
    const first = screen.getByLabelText("Name");
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(screen.getByText("Save cron"));
    fireEvent.keyDown(document.activeElement!, { key: "Tab" });
    expect(document.activeElement).toBe(first);
    screen.getByText("Open creation").focus(); // Programmatic focus escape is contained too.
    expect(document.activeElement).toBe(first);
    fireEvent.click(screen.getByRole("button", { name: "Schedule" }));
    const mode = screen.getByLabelText("Schedule type");
    fireEvent.keyDown(mode, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(screen.getByText("Done"));
    fireEvent.keyDown(document.activeElement!, { key: "Tab" });
    expect(document.activeElement).toBe(mode);
  });
  it("does not close multiple nested dialogs for one Escape", () => {
    const closeOuter = vi.fn(); const closeInner = vi.fn();
    render(() => <Dialog title="Outer" onClose={closeOuter} footer={<button>Outer action</button>}>
      <Dialog title="Inner" onClose={closeInner} footer={<button>Inner action</button>}><input aria-label="Inner field" /></Dialog>
    </Dialog>);
    expect(document.activeElement).toBe(screen.getByLabelText("Inner field"));
    fireEvent.keyDown(screen.getByLabelText("Inner field"), { key: "Escape" });
    expect(closeInner).toHaveBeenCalledTimes(1); expect(closeOuter).not.toHaveBeenCalled();
  });
  it("focuses the modal itself when it has no available controls", () => {
    render(() => <Dialog title="Busy" onClose={() => {}} footer={<button disabled>Saving</button>}>Please wait</Dialog>);
    const dialog = screen.getByRole("dialog");
    expect(document.activeElement).toBe(dialog);
    fireEvent.keyDown(dialog, { key: "Tab" }); expect(document.activeElement).toBe(dialog);
  });
  it("excludes collapsed advanced fields from the focus loop", () => {
    render(() => <Dialog title="Compact" onClose={() => {}} footer={<span>Footer</span>}>
      <input aria-label="First" /><details><summary>Advanced</summary><input aria-label="Hidden override" /></details>
    </Dialog>);
    const first=screen.getByLabelText("First");
    fireEvent.keyDown(first,{key:"Tab",shiftKey:true});
    expect(document.activeElement).toBe(screen.getByText("Advanced"));
    fireEvent.keyDown(document.activeElement!,{key:"Tab"});
    expect(document.activeElement).toBe(first);
  });

});
