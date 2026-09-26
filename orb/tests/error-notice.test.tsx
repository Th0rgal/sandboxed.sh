import { render, screen, fireEvent, waitFor } from "@solidjs/testing-library";
import { describe, it, expect, vi } from "vitest";
import { ErrorNotice } from "../src/ErrorNotice";
describe("inline errors", () => {
  it("summarizes disk admission and preserves the backend details", () => {
    const raw="mission needs an estimated 64 GiB scratch plus a 64 GiB emergency floor (128 GiB required), but only 127 GiB is free at /root (filesystem statvfs:7321850625438636562); select a remote node or free space";
    const dismiss=vi.fn();
    render(()=><ErrorNotice error={raw} onDismiss={dismiss} />);
    expect(screen.getByRole("alert").textContent).toContain("Not enough disk space");
    expect(screen.getByText(/127 GiB available/)).toBeTruthy();
    expect(screen.getByText(raw).closest("details")?.open).toBe(false);
    fireEvent.click(screen.getByRole("button",{name:"Dismiss error"}));expect(dismiss).toHaveBeenCalledOnce();
  });
});

it("copies the complete raw error even when the details are folded", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  const raw = "Internal error: " + "long backend detail ".repeat(40);
  render(() => <ErrorNotice error={raw} />);
  fireEvent.click(screen.getByRole("button", { name: "Copy error" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Error copied" })).toBeTruthy());
  expect(writeText).toHaveBeenCalledWith(raw);
});
it("shows clipboard failures without claiming success", async () => {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error("Permission denied")) } });
  render(() => <ErrorNotice error="Original error" />);
  fireEvent.click(screen.getByRole("button", { name: "Copy error" }));
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Permission denied"));
  expect(screen.queryByRole("button", { name: "Error copied" })).toBeNull();
});
