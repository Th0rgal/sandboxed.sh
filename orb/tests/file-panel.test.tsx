import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { it, expect, vi } from "vitest";
import { FilePanelProvider, FilePanelButton } from "../src/FilePanel";
import { MdView } from "../src/Markdown";
vi.mock("../src/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/api")>()),
  api: vi.fn(async (_path: string, init: RequestInit) => {
    const q = JSON.parse(String(init.body));
    if (q.action === "roots")
      return {
        sources: [{ id: "workspace", label: "Workspace", available: true }],
      };
    if (q.action === "list")
      return { entries: [{ name: "note.md", path: "note.md", kind: "file" }] };
    if (q.action === "resolve")
      return {
        results: q.paths.map((reference: string) => ({
          reference,
          matches:
            reference === "note.md"
              ? [{ name: "note.md", path: "note.md", kind: "file" }]
              : [],
        })),
      };
    if (q.action === "read")
      return {
        content: "# Readable note\n\nEvidence.",
        size: 27,
        binary: false,
        truncated: false,
      };
    return { entries: [] };
  }),
}));
it("opens a detected reference beside a mounted chat and switches Markdown with the focused shortcut", async () => {
  const { container, getByRole, getByText } = render(() => (
    <div class="app">
      <FilePanelProvider scope={{ project: "test" }}>
        <div class="chat">
          <input aria-label="Draft" value="keep this" />
          <MdView text="Read `note.md`." />
          <FilePanelButton />
        </div>
      </FilePanelProvider>
    </div>
  ));
  await waitFor(() =>
    expect(container.querySelector(".file-reference")).not.toBeNull(),
  );
  const draft = getByRole("textbox", { name: "Draft" });
  fireEvent.click(container.querySelector(".file-reference")!);
  await waitFor(() => expect(getByText("Readable note")).toBeTruthy());
  expect(getByRole("textbox", { name: "Draft" })).toBe(draft);
  const panel = container.querySelector<HTMLElement>(".file-panel")!;
  panel.focus();
  fireEvent.keyDown(window, { key: "/", metaKey: true });
  expect(container.querySelector(".file-source-code")).not.toBeNull();
  fireEvent.click(getByRole("button", { name: "Close files" }));
  expect(container.querySelector(".file-panel")).toBeNull();
  expect(draft).toHaveProperty("value", "keep this");
});

it("only resolves completed assistant text and updates changed references", async () => {
  const { createSignal } = await import("solid-js");
  const { Transcript } = await import("../src/Transcript");
  const { FileReferenceContext, FileReference } =
    await import("../src/fileReferenceContext");
  const resolve = vi.fn(async (raw: string) => [
    { source: "workspace", path: raw, name: raw, kind: "file" },
  ]);
  const [live, setLive] = createSignal(true);
  const [raw, setRaw] = createSignal("first.md");
  const { container } = render(() => (
    <FileReferenceContext.Provider
      value={{ resolve, open: () => {}, search: () => {} }}
    >
      <Transcript
        items={[{ kind: "text", text: "Read `note.md`.", live: live() }]}
      />
      <FileReference raw={raw()} />
    </FileReferenceContext.Provider>
  ));
  await waitFor(() => expect(resolve).toHaveBeenCalledWith("first.md"));
  expect(resolve).not.toHaveBeenCalledWith("note.md");
  setLive(false);
  await waitFor(() => expect(resolve).toHaveBeenCalledWith("note.md"));
  setRaw("second.md");
  await waitFor(() => expect(resolve).toHaveBeenCalledWith("second.md"));
  await waitFor(() =>
    expect(
      container.querySelector('[title="workspace: second.md"]'),
    ).not.toBeNull(),
  );
});
