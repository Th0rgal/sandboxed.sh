import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { Composer } from "../src/App";
import * as uploads from "../src/uploads";
vi.mock("../src/uploads", async (original) => ({ ...await original<typeof import("../src/uploads")>(), hasNativePicker: () => true, pickNativeFiles: vi.fn(), transferFile: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
it("opens an upload choice with no project context and inserts a real local path", async () => {
  vi.mocked(uploads.pickNativeFiles).mockResolvedValue([{ name: "photo.png", localPath: "/Users/test/photo.png" }]);
  vi.mocked(uploads.transferFile).mockResolvedValue({ source: { name: "photo.png", localPath: "/Users/test/photo.png" }, path: "/Users/test/photo.png", destination: "local", connection: 0 });
  const send = vi.fn();
  render(() => <Composer placeholder="Task" busy={false} onSend={send} onStop={() => {}} uploadTarget="local" picker={false} />);
  fireEvent.click(screen.getByTitle("Add context"));
  fireEvent.click(screen.getByText("Upload file or image…"));
  await waitFor(() => expect((screen.getByPlaceholderText("Task") as HTMLTextAreaElement).value).toBe("@/Users/test/photo.png "));
  expect(uploads.transferFile).toHaveBeenCalledWith(expect.objectContaining({ name: "photo.png" }), "local");
  fireEvent.click(screen.getByTitle("Send"));
  await waitFor(() => expect(send).toHaveBeenCalledWith("@/Users/test/photo.png"));
});
it("keeps the draft when transferring the file fails", async () => {
  vi.mocked(uploads.pickNativeFiles).mockResolvedValue([{ name: "photo.png", localPath: "/Users/test/photo.png" }]);
  vi.mocked(uploads.transferFile).mockRejectedValue(new Error("Machine offline"));
  render(() => <Composer placeholder="Task" busy={false} onSend={() => {}} onStop={() => {}} uploadTarget="ashur" picker={false} />);
  fireEvent.input(screen.getByPlaceholderText("Task"), { target: { value: "Keep my draft" } });
  fireEvent.click(screen.getByTitle("Add context")); fireEvent.click(screen.getByText("Upload file or image…"));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toBe("Machine offline"));
  expect((screen.getByPlaceholderText("Task") as HTMLTextAreaElement).value).toBe("Keep my draft");
});
