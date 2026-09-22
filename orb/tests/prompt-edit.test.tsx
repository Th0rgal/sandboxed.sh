import { render, fireEvent, screen, cleanup } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { UserTurn } from "../src/Transcript";
afterEach(cleanup);
it("double-click edits a draft and reuses it without changing the historic prompt", async () => {
  const reuse = vi.fn();
  render(() => <UserTurn text="Original prompt" onReuse={reuse} />);
  fireEvent.dblClick(screen.getByText("Original prompt"));
  const editor = screen.getByRole("textbox", { name: "Edit prompt text" });
  fireEvent.input(editor, { target: { value: "Updated prompt" } });
  fireEvent.click(screen.getByRole("button", { name: "Use as follow-up" }));
  expect(reuse).toHaveBeenCalledWith("Updated prompt");
  expect(screen.getByText("Original prompt")).toBeTruthy();
});
it("escape cancels without submitting", () => {
  const reuse = vi.fn();
  render(() => <UserTurn text="Keep this" onReuse={reuse} />);
  fireEvent.click(screen.getByRole("button", { name: "Edit prompt" }));
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
  expect(reuse).not.toHaveBeenCalled();
  expect(screen.queryByRole("textbox")).toBeNull();
});
