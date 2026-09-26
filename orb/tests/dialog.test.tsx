import { describe, it, expect, vi } from "vitest";
import { createSignal, Show } from "solid-js";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { ConfirmDialog, Dialog, PromptSheet } from "../src/Dialog";
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
    const close = screen.getByRole("button", { name: "Close" }); close.focus();
    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(screen.getByText("Save cron"));
    fireEvent.keyDown(document.activeElement!, { key: "Tab" });
    expect(document.activeElement).toBe(close);
    screen.getByText("Open creation").focus(); // Programmatic focus escape is contained too.
    expect(document.activeElement).toBe(close);
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
    render(() => <Dialog title="Busy" busy onClose={() => {}} footer={<button disabled>Saving</button>}>Please wait</Dialog>);
    const dialog = screen.getByRole("dialog");
    expect(document.activeElement).toBe(dialog);
    fireEvent.keyDown(dialog, { key: "Tab" }); expect(document.activeElement).toBe(dialog);
  });
  it("excludes collapsed advanced fields from the focus loop", () => {
    render(() => <Dialog title="Compact" onClose={() => {}} footer={<span>Footer</span>}>
      <input aria-label="First" /><details><summary>Advanced</summary><input aria-label="Hidden override" /></details>
    </Dialog>);
    const first=screen.getByLabelText("First");
    const close = screen.getByRole("button", { name: "Close" }); close.focus();
    fireEvent.keyDown(close,{key:"Tab",shiftKey:true});
    expect(document.activeElement).toBe(screen.getByText("Advanced"));
    fireEvent.keyDown(document.activeElement!,{key:"Tab"});
    expect(document.activeElement).toBe(close);
  });

});

describe("shared modal behavior", () => {
  it("requires a complete outside pointer gesture and blocks dismissal while busy", () => {
    const close = vi.fn();
    const [busy, setBusy] = createSignal(false);
    render(() => <Dialog title="Saving" busy={busy()} onClose={close}>Content</Dialog>);
    const panel = screen.getByRole("dialog");
    const backdrop = panel.parentElement!;
    fireEvent.pointerDown(panel); fireEvent.pointerUp(backdrop);
    expect(close).not.toHaveBeenCalled();
    fireEvent.pointerDown(backdrop); fireEvent.pointerUp(panel);
    expect(close).not.toHaveBeenCalled();
    fireEvent.pointerDown(backdrop); fireEvent.pointerUp(backdrop);
    expect(close).toHaveBeenCalledTimes(1);
    setBusy(true);
    fireEvent.pointerDown(backdrop); fireEvent.pointerUp(backdrop);
    fireEvent.keyDown(panel, { key: "Escape" });
    expect((screen.getByRole("button", { name: "Close" }) as HTMLButtonElement).disabled).toBe(true);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("keeps a rejected naming form open and allows one retry after busy clears", () => {
    const action = vi.fn(() => setBusy(true));
    const [busy, setBusy] = createSignal(false);
    const [error, setError] = createSignal<string | null>(null);
    const [value, setValue] = createSignal("Notes");
    render(() => <PromptSheet title="Rename" value={value()} onInput={setValue} action="Save"
      busy={busy()} error={error()} onAction={action} onClose={() => {}} />);
    const input = screen.getByRole("textbox");
    expect(document.activeElement).toBe(input);
    fireEvent.submit(input.closest("form")!); fireEvent.submit(input.closest("form")!);
    expect(action).toHaveBeenCalledTimes(1);
    setBusy(false); setError("Try again");
    expect(screen.getByRole("alert").textContent).toContain("Try again");
    expect((input as HTMLInputElement).value).toBe("Notes");
    fireEvent.submit(input.closest("form")!);
    expect(action).toHaveBeenCalledTimes(2);
  });

  it("stacks confirmations without double dimming and restores both focus and scrolling", async () => {
    const originalOverflow = document.body.style.overflow;
    document.body.style.overflow = "auto";
    function Nested() {
      const [open, setOpen] = createSignal(false);
      const [confirm, setConfirm] = createSignal(false);
      return <><button onClick={() => setOpen(true)}>Open</button>
        <Show when={open()}><Dialog title="Form" onClose={() => setOpen(false)}>
          <button onClick={() => setConfirm(true)}>Discard</button>
          <Show when={confirm()}><ConfirmDialog title="Discard?" description="Unsaved changes" action="Discard draft"
            destructive onConfirm={() => setOpen(false)} onClose={() => setConfirm(false)} /></Show>
        </Dialog></Show></>;
    }
    render(() => <Nested />);
    const opener = screen.getByText("Open"); opener.focus(); fireEvent.click(opener);
    const discard = screen.getByText("Discard"); discard.focus(); fireEvent.click(discard);
    expect(document.activeElement).toBe(screen.getByText("Cancel"));
    expect(document.querySelectorAll(".dlg-back-dim")).toHaveLength(1);
    expect(screen.getByRole("dialog", { name: "Form" }).inert).toBe(true);
    expect(document.body.style.overflow).toBe("hidden");
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(document.activeElement).toBe(discard));
    expect(screen.getByRole("dialog", { name: "Form" }).inert).toBe(false);
    fireEvent.keyDown(discard, { key: "Escape" });
    expect(document.activeElement).toBe(opener);
    expect(document.body.style.overflow).toBe("auto");
    document.body.style.overflow = originalOverflow;
  });
});
