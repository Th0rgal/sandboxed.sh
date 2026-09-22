import { describe, expect, it } from "vitest";
import { isSecretPath, quotePath, rewritePrompt, materializeMentions } from "../src/localAgents";
import type { AttachChip } from "../src/attach";

describe("local mention rewrite", () => {
  it("leaves unknown @words and rewrites a copied file", () => {
    const text = "see @notes/foo.md and email me @home";
    const out = rewritePrompt(text, [{ raw: "@notes/foo.md", absolute: "/tmp/orb/notes/foo.md" }]);
    expect(out).toBe("see /tmp/orb/notes/foo.md and email me @home");
  });

  it("quotes paths that contain spaces", () => {
    expect(quotePath("/tmp/my files/a.md")).toBe('"/tmp/my files/a.md"');
  });

  it("refuses secrets and unreadable files, and copies a readable one", async () => {
    expect(isSecretPath("notes/.env")).toBe(true);
    expect(isSecretPath("notes/foo.md")).toBe(false);
    const chips: AttachChip[] = [
      { id: "f", kind: "file", path: "notes/foo.md", label: "foo" },
      { id: "s", kind: "file", path: ".env", label: "env" },
    ];
    const ok = await materializeMentions(
      "demo",
      "read @notes/foo.md",
      chips,
      async () => "hello",
      async () => [],
    );
    expect(ok.files).toEqual([{ rel: ".paloma/attach/notes/foo.md", content: "hello" }]);
    expect(ok.prompt).toContain("__ROOT__/.paloma/attach/notes/foo.md");
    await expect(materializeMentions("demo", "read @.env", chips, async () => "", async () => [])).rejects.toThrow(/not copied/);
    await expect(
      materializeMentions("demo", "read @notes/foo.md", chips, async () => {
        throw new Error("missing");
      }, async () => []),
    ).rejects.toThrow(/could not be read/);
  });
});
