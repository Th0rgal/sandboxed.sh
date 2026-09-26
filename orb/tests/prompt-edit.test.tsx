import { render, fireEvent, screen, cleanup, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { UserTurn } from "../src/Transcript";
afterEach(cleanup);
it("double-click edits a draft and reuses it without changing the historic prompt", async () => {
  const reuse = vi.fn().mockResolvedValue(true);
  render(() => <UserTurn text="Original prompt" onSend={reuse} />);
  fireEvent.dblClick(screen.getByText("Original prompt"));
  const editor = screen.getByRole("textbox", { name: "Edit prompt text" });
  fireEvent.input(editor, { target: { value: "Updated prompt" } });
  fireEvent.click(screen.getByRole("button", { name: "Send follow-up" }));
  expect(reuse).toHaveBeenCalledWith("Updated prompt");
  await waitFor(() => expect(screen.queryByRole("textbox")).toBeNull());
  expect(screen.getByText("Original prompt")).toBeTruthy();
});
it("escape cancels without submitting", () => {
  const reuse = vi.fn().mockResolvedValue(true);
  render(() => <UserTurn text="Keep this" onSend={reuse} />);
  fireEvent.click(screen.getByRole("button", { name: "Edit prompt" }));
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
  expect(reuse).not.toHaveBeenCalled();
  expect(screen.queryByRole("textbox")).toBeNull();
});
it("awaits dispatch, disables duplicate sends and preserves the draft on failure", async () => {
  let reject!: (e: Error) => void;
  const send = vi.fn(() => new Promise<boolean>((_, r) => { reject = r; }));
  render(() => <UserTurn text="Original" onSend={send} />);
  fireEvent.click(screen.getByRole("button", {name:"Edit prompt"}));
  fireEvent.input(screen.getByRole("textbox"), {target:{value:"Retry me"}});
  fireEvent.click(screen.getByRole("button", {name:"Send follow-up"}));
  expect(screen.getByRole("button", {name:"Sending follow-up"})).toHaveProperty("disabled",true);
  fireEvent.click(screen.getByRole("button", {name:"Sending follow-up"}));
  expect(send).toHaveBeenCalledTimes(1);
  reject(new Error("Network unavailable"));
  await screen.findByText("Network unavailable");
  expect(screen.getByRole("textbox")).toHaveProperty("value","Retry me");
  expect(screen.getByRole("button", {name:"Send follow-up"})).toHaveProperty("disabled",false);
});
it("supports Ctrl+Enter and keeps declined sends editable", async () => {
  const send=vi.fn().mockResolvedValue(false);
  const {container}=render(() => <UserTurn text="Original" onSend={send} />);
  fireEvent.click(screen.getByRole("button", {name:"Edit prompt"}));
  expect(container.querySelector('.user.editing')).toBeTruthy();
  fireEvent.keyDown(screen.getByRole("textbox"), {key:"Enter",ctrlKey:true});
  await screen.findByText("The message was not sent. Your draft is kept; try again.");
  expect(send).toHaveBeenCalledWith("Original");
  expect(screen.getByRole("textbox")).toHaveProperty("value","Original");
});
