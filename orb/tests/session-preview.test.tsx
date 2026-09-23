import { render, screen, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionPreview, type SessionPreviewData } from "../src/SessionPreview";
import { localSessionGit } from "../src/localAgents";
vi.mock("../src/localAgents", () => ({
  localSessionGit: vi
    .fn()
    .mockResolvedValue({ repository: "verity", branch: "feat/preview" }),
}));
const base: SessionPreviewData = {
  id: "a",
  title: "Audit",
  local: true,
  destination: "This computer",
  directory: "/work/verity",
  model: "Opus 5.5",
  harness: "Claude Code",
  effort: "High",
  context: 12,
};
afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});
describe("session title preview", () => {
  it("opens after a short hover, fetches local Git once, and remains hoverable", async () => {
    vi.useFakeTimers();
    render(() => <SessionPreview data={base} />);
    const title = screen.getByRole("button", {
      name: "Session details: Audit",
    });
    fireEvent.pointerEnter(title);
    expect(screen.queryByRole("tooltip")).toBeNull();
    await vi.advanceTimersByTimeAsync(220);
    expect(screen.getByText("feat/preview")).toBeTruthy();
    expect(screen.getByText("/work/verity")).toBeTruthy();
    expect(screen.getByText(/≈ 12% context/)).toBeTruthy();
    fireEvent.pointerLeave(title);
    fireEvent.pointerEnter(screen.getByRole("tooltip"));
    await vi.advanceTimersByTimeAsync(200);
    expect(screen.getByRole("tooltip")).toBeTruthy();
    fireEvent.pointerLeave(screen.getByRole("tooltip"));
    await vi.advanceTimersByTimeAsync(150);
    expect(screen.queryByRole("tooltip")).toBeNull();
    fireEvent.focus(title);
    expect(localSessionGit).toHaveBeenCalledTimes(1);
  });
  it("supports keyboard focus and Escape without triggering parent navigation", () => {
    render(() => <SessionPreview data={base} />);
    const title = screen.getByRole("button");
    fireEvent.focus(title);
    expect(title.getAttribute("aria-describedby")).toBe(
      screen.getByRole("tooltip").id,
    );
    fireEvent.keyDown(title, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).toBeNull();
  });
  it("updates context without dismissing and closes when the session changes", () => {
    const [data, setData] = createSignal(base);
    render(() => <SessionPreview data={data()} />);
    fireEvent.focus(screen.getByRole("button"));
    setData({ ...base, context: 20 });
    expect(screen.getByRole("tooltip")).toBeTruthy();
    expect(screen.getByText(/≈ 20% context/)).toBeTruthy();
    setData({ ...base, id: "b", title: "Different" });
    expect(screen.queryByRole("tooltip")).toBeNull();
  });
  it("does not probe local Git for remote sessions or invent unknown metadata", () => {
    render(() => (
      <SessionPreview
        data={{
          id: "remote",
          title: "Remote",
          local: false,
          destination: "Ashur",
        }}
      />
    ));
    fireEvent.focus(screen.getByRole("button"));
    expect(localSessionGit).not.toHaveBeenCalled();
    expect(screen.getByText("Ashur")).toBeTruthy();
    expect(screen.queryByText(/context/)).toBeNull();
    expect(screen.queryByText("Default model")).toBeNull();
  });
  it("ignores a late Git response from a previously selected session", async () => {
    let resolve!: (value: { repository: string }) => void;
    vi.mocked(localSessionGit).mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const [data, setData] = createSignal(base);
    render(() => <SessionPreview data={data()} />);
    fireEvent.focus(screen.getByRole("button"));
    setData({
      id: "remote",
      title: "Remote",
      local: false,
      destination: "Core",
    });
    fireEvent.focus(screen.getByRole("button"));
    resolve({ repository: "stale-repository" });
    await Promise.resolve();
    expect(screen.queryByText("stale-repository")).toBeNull();
  });
});
