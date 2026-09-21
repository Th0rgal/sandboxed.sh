import { describe, expect, it } from "vitest";
import {
  CONTROLLER_MENTION,
  insertMention,
  mentionText,
  mentionedChips,
  scanMentions,
  type AttachItem,
} from "../src/attach";

const file = (path: string): AttachItem => ({ id: `file:${path}`, kind: "file", section: "Files", path, label: path });
const folder = (path: string): AttachItem => ({ id: `folder:${path}`, kind: "folder", section: "Folders", path, label: `${path}/` });
const controller: AttachItem = { id: "controller:pareto", kind: "controller", section: "Controller", label: "Pareto controller" };

const ITEMS: AttachItem[] = [
  file("notes.md"),
  file("reference/spec.md"),
  // Same basename, different folders — only the full path tells them apart.
  file("audit/spec.md"),
  file("reference/my notes.md"),
  folder("reference"),
  controller,
];
const ids = (text: string) => mentionedChips(text, ITEMS).map((c) => c.id);

describe("how a mention is written", () => {
  it("uses the full path, so two files of the same name stay distinct", () => {
    expect(mentionText(file("reference/spec.md"))).toBe("@reference/spec.md");
    expect(mentionText(file("audit/spec.md"))).toBe("@audit/spec.md");
  });

  it("quotes a path containing spaces", () => {
    expect(mentionText(file("reference/my notes.md"))).toBe('@"reference/my notes.md"');
  });

  it("marks a folder with a trailing slash and names the controller", () => {
    expect(mentionText(folder("reference"))).toBe("@reference/");
    expect(mentionText(controller)).toBe(`@${CONTROLLER_MENTION}`);
  });
});

describe("inserting at the caret", () => {
  it("replaces the @query being typed and leaves the caret after a space", () => {
    const text = "compare @ref with the rest";
    const caret = "compare @ref".length;
    const next = insertMention(text, caret, file("reference/spec.md"));
    expect(next.text).toBe("compare @reference/spec.md  with the rest");
    expect(next.caret).toBe("compare @reference/spec.md ".length);
    // Typing continues mid-sentence, not in a pill above the box.
    expect(next.text.slice(next.caret)).toBe(" with the rest");
  });

  it("inserts at the caret when there is no @query (menu button)", () => {
    const next = insertMention("look at ", 8, file("notes.md"));
    expect(next.text).toBe("look at @notes.md ");
    expect(next.caret).toBe(next.text.length);
  });
});

describe("what a draft actually attaches", () => {
  it("resolves several different attachments inside one sentence", () => {
    expect(ids("diff @reference/spec.md against @audit/spec.md and note @notes.md"))
      .toEqual(["file:reference/spec.md", "file:audit/spec.md", "file:notes.md"]);
  });

  it("keeps sentence order and sends a repeated mention once", () => {
    expect(ids("@notes.md then @reference/spec.md then @notes.md again"))
      .toEqual(["file:notes.md", "file:reference/spec.md"]);
  });

  it("resolves a quoted path with spaces", () => {
    expect(ids('read @"reference/my notes.md" closely')).toEqual(["file:reference/my notes.md"]);
  });

  it("detaches a mention the user deleted", () => {
    const withBoth = "check @notes.md and @audit/spec.md";
    expect(ids(withBoth)).toHaveLength(2);
    // Deleting the text is the whole gesture — nothing attaches invisibly.
    expect(ids(withBoth.replace(" and @audit/spec.md", ""))).toEqual(["file:notes.md"]);
    expect(ids("check nothing")).toEqual([]);
  });

  it("re-attaches when the mention is typed back", () => {
    expect(ids("plain sentence")).toEqual([]);
    expect(ids("plain sentence @notes.md")).toEqual(["file:notes.md"]);
  });

  it("treats an unknown @word as prose, not an attachment", () => {
    expect(ids("ask @someone about @nope.md")).toEqual([]);
    expect(ids("mail me at me@example.com")).toEqual([]);
  });

  it("does not swallow trailing sentence punctuation", () => {
    expect(ids("please read @notes.md.")).toEqual(["file:notes.md"]);
    expect(ids("both @notes.md, @audit/spec.md; thanks")).toEqual(["file:notes.md", "file:audit/spec.md"]);
    expect(ids("(see @notes.md)")).toEqual(["file:notes.md"]);
  });

  it("resolves folders and the controller", () => {
    expect(ids("scan @reference/ and @controller")).toEqual(["folder:reference", "controller:pareto"]);
  });

  it("carries the identity needed for the payload", () => {
    const chips = mentionedChips("@reference/spec.md and @controller", ITEMS);
    expect(chips[0]).toMatchObject({ kind: "file", path: "reference/spec.md" });
    expect(chips[1]).toMatchObject({ kind: "controller" });
    expect(chips[1].path).toBeUndefined();
  });
});

describe("scanning", () => {
  it("reports each mention with the text it occupies", () => {
    const found = scanMentions('a @notes.md b @"reference/my notes.md" c');
    expect(found.map((m) => m.value)).toEqual(["notes.md", "reference/my notes.md"]);
    expect(found[0].raw).toBe("@notes.md");
    expect(found[1].raw).toBe('@"reference/my notes.md"');
    expect('a @notes.md b @"reference/my notes.md" c'.slice(found[1].index, found[1].index + found[1].raw.length))
      .toBe('@"reference/my notes.md"');
  });

  it("only matches at a token start", () => {
    expect(scanMentions("user@host and a@b.md")).toEqual([]);
    expect(scanMentions("@notes.md").map((m) => m.value)).toEqual(["notes.md"]);
  });
});

it("keeps case-sensitive reference paths distinct", () => {
  const files = [file("Notes.md"), file("notes.md")];
  expect(mentionedChips("@Notes.md @notes.md", files).map(c => c.path)).toEqual(["Notes.md", "notes.md"]);
});
