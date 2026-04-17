import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  getBuiltinCommands,
  getVisibleAgents,
  listLibraryCommands,
} from "@/lib/api";
import { EnhancedInput } from "./enhanced-input";

vi.mock("@/lib/api", () => ({
  listLibraryCommands: vi.fn().mockResolvedValue([]),
  getBuiltinCommands: vi
    .fn()
    .mockResolvedValue({ opencode: [], claudecode: [] }),
  getVisibleAgents: vi.fn().mockResolvedValue([]),
}));

beforeEach(() => {
  vi.mocked(listLibraryCommands).mockResolvedValue([]);
  vi.mocked(getVisibleAgents).mockResolvedValue([]);
  vi.mocked(getBuiltinCommands).mockResolvedValue({
    opencode: [],
    claudecode: [],
  });
});

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

describe("EnhancedInput composer behavior", () => {
  it("does not clear text synchronously when submitted", async () => {
    const onSubmit = vi.fn();

    const { container } = render(
      <EnhancedInput
        value="draft message"
        onChange={() => {}}
        onSubmit={onSubmit}
      />,
    );
    await Promise.resolve();

    const textarea = container.querySelector("textarea");
    expect(textarea).not.toBeNull();

    fireEvent.keyDown(textarea as HTMLTextAreaElement, {
      key: "Enter",
      code: "Enter",
      shiftKey: false,
    });

    expect(onSubmit).toHaveBeenCalledWith({ content: "draft message" });
    expect((textarea as HTMLTextAreaElement).value).toBe("draft message");
  });

  it("reports submit availability during the input change event", async () => {
    const onCanSubmitChange = vi.fn();

    function ControlledInput() {
      const [value, setValue] = useState("");
      return (
        <EnhancedInput
          value={value}
          onChange={setValue}
          onSubmit={() => {}}
          onCanSubmitChange={onCanSubmitChange}
        />
      );
    }

    const { container } = render(<ControlledInput />);
    await Promise.resolve();

    const textarea = container.querySelector("textarea");
    expect(textarea).not.toBeNull();

    fireEvent.change(textarea as HTMLTextAreaElement, {
      target: { value: "hello" },
    });

    expect(onCanSubmitChange).toHaveBeenLastCalledWith(true);
  });
});

describe("EnhancedInput command autocomplete backend filtering", () => {
  it("does not show OpenCode or Claude builtins for codex backend", async () => {
    vi.mocked(getBuiltinCommands).mockResolvedValue({
      opencode: [{ name: "ralph-loop", description: null, path: "builtin" }],
      claudecode: [{ name: "plan", description: null, path: "builtin-claude" }],
    });

    function ControlledInput() {
      const [value, setValue] = useState("");
      return (
        <EnhancedInput
          value={value}
          onChange={setValue}
          onSubmit={() => {}}
          backend="codex"
        />
      );
    }

    const { container } = render(<ControlledInput />);
    const textarea = container.querySelector("textarea");
    expect(textarea).not.toBeNull();

    fireEvent.change(textarea as HTMLTextAreaElement, { target: { value: "/" } });

    await waitFor(() => {
      expect(screen.queryByText("plan")).not.toBeInTheDocument();
      expect(screen.queryByText("ralph-loop")).not.toBeInTheDocument();
    });
  });

  it("shows only Claude builtins for claudecode backend", async () => {
    vi.mocked(getBuiltinCommands).mockResolvedValue({
      opencode: [{ name: "ralph-loop", description: null, path: "builtin" }],
      claudecode: [{ name: "plan", description: null, path: "builtin-claude" }],
    });

    function ControlledInput() {
      const [value, setValue] = useState("");
      return (
        <EnhancedInput
          value={value}
          onChange={setValue}
          onSubmit={() => {}}
          backend="claudecode"
        />
      );
    }

    const { container } = render(<ControlledInput />);
    const textarea = container.querySelector("textarea");
    expect(textarea).not.toBeNull();

    fireEvent.change(textarea as HTMLTextAreaElement, { target: { value: "/" } });

    expect(await screen.findByText("plan")).toBeInTheDocument();
    expect(screen.queryByText("ralph-loop")).not.toBeInTheDocument();
  });
});
