import { describe, expect, it } from "vitest";
import { missionCopyId, newFilePath, REFERENCE_FILE_EXT } from "../src/ProjectFiles";
import { copyText } from "../src/clipboard";

describe("Copy mission ID copies the sandboxed mission id", () => {
  const mission = {
    id: "3f2a91c4-8b7d-4e21-9a0c-5d6e7f801234",
    status: "active",
    title: "Lido SRv3 report",
    history: [],
    // Everything below identifies an execution attempt, not the mission.
    remote_job: { job_id: "job_9f81c0aa", node_id: "dgx-spark", phase: "running" },
    remote_node_id: "dgx-spark",
    workspace_name: "lido-srv3",
    created_at: "",
    updated_at: "",
  };

  it("returns the raw UUID, not the sidebar prefix or an execution id", () => {
    const copied = missionCopyId(mission);
    expect(copied).toBe("3f2a91c4-8b7d-4e21-9a0c-5d6e7f801234");
    expect(copied.startsWith("m:")).toBe(false);
    expect(copied).not.toBe(mission.remote_job.job_id);
    expect(copied).not.toBe(mission.remote_node_id);
    expect(copied).not.toBe(mission.workspace_name);
  });

  it("is the id the mission API path is built from", () => {
    // `m:` is only the sidebar routing key; slicing it must round-trip.
    expect(`m:${missionCopyId(mission)}`.slice(2)).toBe(mission.id);
  });
});

describe("clipboard failures surface instead of looking like a success", () => {
  const original = navigator.clipboard;
  const setClipboard = (value: unknown) =>
    Object.defineProperty(navigator, "clipboard", { value, configurable: true });

  it("writes through when the platform allows it", async () => {
    const writes: string[] = [];
    setClipboard({ writeText: async (t: string) => void writes.push(t) });
    await copyText("abc");
    expect(writes).toEqual(["abc"]);
    setClipboard(original);
  });

  it("throws a message when the clipboard is missing", async () => {
    setClipboard(undefined);
    await expect(copyText("abc")).rejects.toThrow(/unavailable/i);
    setClipboard(original);
  });

  it("throws a message when the platform refuses the write", async () => {
    setClipboard({ writeText: async () => { throw new Error("NotAllowedError"); } });
    await expect(copyText("abc")).rejects.toThrow(/refused.*NotAllowedError/i);
    setClipboard(original);
  });
});

describe("new reference file paths", () => {
  it("resolves a name against the folder it was invoked on", () => {
    expect(newFilePath("reference", "spec")).toEqual({ path: "reference/spec.md", name: "spec.md" });
    expect(newFilePath("", "readme")).toEqual({ path: "readme.md", name: "readme.md" });
    expect(newFilePath("a/b", "notes")).toEqual({ path: "a/b/notes.md", name: "notes.md" });
  });

  it("defaults to Markdown only when there is no extension", () => {
    expect(REFERENCE_FILE_EXT).toBe(".md");
    expect(newFilePath("", "plan.md")).toEqual({ path: "plan.md", name: "plan.md" });
    expect(newFilePath("", "data.json")).toEqual({ path: "data.json", name: "data.json" });
    expect(newFilePath("", ".gitignore")).toEqual({ path: ".gitignore", name: ".gitignore" });
  });

  it("refuses empty names and traversal", () => {
    expect(newFilePath("reference", "")).toEqual({ error: "Enter a file name." });
    expect(newFilePath("reference", "   ")).toEqual({ error: "Enter a file name." });
    for (const bad of ["../escape", "a/../../b", "..", "."]) {
      expect(newFilePath("reference", bad)).toHaveProperty("error");
    }
    expect(newFilePath("reference", "/abs")).toHaveProperty("error");
    expect(newFilePath("reference", "a\\b")).toHaveProperty("error");
    expect(newFilePath("reference", "a//b")).toHaveProperty("error");
  });

  it("never produces a path the core's safe_join would reject", () => {
    const ok = newFilePath("reference", "sub/notes");
    expect(ok).toEqual({ path: "reference/sub/notes.md", name: "notes.md" });
    expect("path" in ok && ok.path.split("/").every((part) => part && part !== "." && part !== "..")).toBe(true);
  });
});
