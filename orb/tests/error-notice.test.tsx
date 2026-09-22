import { render, screen, fireEvent } from "@solidjs/testing-library";
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
