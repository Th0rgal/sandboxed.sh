import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { EnhancedInput } from "./enhanced-input";

vi.mock("@/lib/api", () => ({
  listLibraryCommands: vi.fn().mockResolvedValue([]),
  getBuiltinCommands: vi
    .fn()
    .mockResolvedValue({ opencode: [], claudecode: [] }),
  getVisibleAgents: vi.fn().mockResolvedValue([]),
}));

describe("EnhancedInput file paste handling", () => {
  it("passes textarea selection to onFilePaste", async () => {
    const onFilePaste = vi.fn();
    const file = new File(["img"], "paste.png", { type: "image/png" });
    const fileItem = {
      kind: "file",
      getAsFile: () => file,
    };

    const { container, unmount } = render(
      <EnhancedInput
        value={"hello world"}
        onChange={() => {}}
        onSubmit={() => {}}
        onFilePaste={onFilePaste}
      />,
    );
    // Let async command/agent loading effects settle before teardown.
    await Promise.resolve();
    await Promise.resolve();

    const textarea = container.querySelector("textarea");
    expect(textarea).not.toBeNull();
    textarea!.setSelectionRange(6, 11);

    fireEvent.paste(textarea as HTMLTextAreaElement, {
      clipboardData: {
        items: [fileItem],
        getData: () => "",
      },
    });

    expect(onFilePaste).toHaveBeenCalledTimes(1);
    expect(onFilePaste).toHaveBeenCalledWith([file], {
      selectionStart: 6,
      selectionEnd: 11,
    });

    unmount();
    await Promise.resolve();
  });
});
