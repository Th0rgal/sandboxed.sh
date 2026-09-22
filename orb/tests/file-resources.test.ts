import { describe, it, expect } from "vitest";
import {
  parseFileTarget,
  splitFileReferences,
  relativeFilePath,
} from "../src/fileResources";
describe("file references", () => {
  it.each([
    "audit/guarantees.yaml",
    "/workspaces/mission/repo/notes.md",
    "proof.lean:42",
    "proof.lean:42:7",
    "proof.lean#L42",
    "A folder/Note.md",
  ])("recognizes %s", (raw) => expect(parseFileTarget(raw)).not.toBeNull());
  it.each([
    "https://example.com/file.md",
    "javascript:alert(1)",
    "file:///etc/passwd",
    "/srv/.../note.md",
    "/srv/…/note.md",
    "hello world",
  ])("rejects %s", (raw) => expect(parseFileTarget(raw)).toBeNull());
  it("separates line from identity", () =>
    expect(parseFileTarget("audit/proof.lean:42:7")).toEqual({
      path: "audit/proof.lean",
      line: 42,
    }));
  it("never resolves paths inside web URLs", () =>
    expect(
      splitFileReferences("See https://example.com/a/file.md now").filter(
        (p) => p.target,
      ),
    ).toHaveLength(0));
  it("preserves prose byte for byte", () => {
    const text = "Read audit/guarantees.yaml and `notes.md`.";
    expect(
      splitFileReferences(text)
        .map((p) => p.text)
        .join(""),
    ).toBe(text);
  });
});

it("resolves document links without escaping the source", () => {
  expect(relativeFilePath("audit/notes.md", "../README.md")).toBe("README.md");
  expect(relativeFilePath("notes.md", "../secret.md")).toBeNull();
  expect(relativeFilePath("audit/notes.md", "/workspace/README.md")).toBe(
    "/workspace/README.md",
  );
});
