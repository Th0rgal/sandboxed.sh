import { test, expect, type Page } from "@playwright/test";

const ENTRIES = [
  { name: "notes.md", kind: "file" },
  { name: "reference", kind: "dir" },
  { name: "audit", kind: "dir" },
];
const SUB: Record<string, Array<{ name: string; kind: string }>> = {
  reference: [{ name: "spec.md", kind: "file" }, { name: "my notes.md", kind: "file" }],
  audit: [{ name: "spec.md", kind: "file" }],
};

async function setup(page: Page, filesReady: Promise<void> = Promise.resolve()) {
  const posts: any[] = [];
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
    localStorage.setItem("orb.harnessPick", JSON.stringify({ backend: "codex", model: "gpt-6-astra" }));
  });
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    if(path === "/api/model-routing/chains") return route.fulfill({json:[{id:"builtin/smart",name:"Smart (Default)"}]});
    if (path === "/api/control/missions" && request.method() === "POST") {
      posts.push(request.postDataJSON());
      return route.fulfill({ status: 503, body: "Runner admission unavailable" });
    }
    if (path.endsWith("/files")) {
      await filesReady;
      const dir = url.searchParams.get("path") ?? "";
      return route.fulfill({ json: { entries: dir ? (SUB[dir] ?? []) : ENTRIES } });
    }
    const json =
      path === "/api/projects" ? { projects: [{ slug: "test", title: "Test" }] }
      : path === "/api/backends" ? [{ id: "codex", name: "Codex" }]
      : path === "/api/providers/backend-models" ? { backends: { codex: [{ value: "gpt-6-astra", label: "OpenAI — GPT-6 Astra" }] } }
      : path === "/api/remote-nodes" ? { enabled: true, nodes: [] }
      : path === "/api/control/missions" ? []
      : path.endsWith("/crons") ? { jobs: [] }
      : { job: null, runs: [] };
    await route.fulfill({ json });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Choose project", exact: true }).click();
  await page.getByRole("option", { name: "Test", exact: true }).click();
  const input = page.locator(".new-agent .composer textarea");
  await expect(input).toBeVisible();
  return { posts, input };
}

/**
 * Type `@query`, then choose the offered item. The query itself cannot contain
 * a space — whitespace closes the picker — so a path with spaces is found by a
 * fragment and written back in quoted form.
 */
async function mention(page: Page, input: ReturnType<Page["locator"]>, query: string, name: string | RegExp) {
  await input.pressSequentially(`@${query}`);
  const menu = page.getByRole("listbox", { name: "Context" });
  await expect(menu).toBeVisible();
  await menu.getByRole("option", { name }).first().click();
}

test("mentions land inline where @ was typed, not in pills above the box", async ({ page }) => {
  const { input } = await setup(page);

  await input.pressSequentially("compare ");
  await mention(page, input, "reference/spec", /reference\/spec\.md/);
  await input.pressSequentially("with ");
  await mention(page, input, "audit/spec", /audit\/spec\.md/);
  await input.pressSequentially("and ignore the rest");

  // Both references sit in the sentence, in the order they were written, and
  // the full paths keep two files of the same name apart.
  await expect(input).toHaveValue("compare @reference/spec.md with @audit/spec.md and ignore the rest");
  // Nothing is grouped above the textarea any more.
  await expect(page.locator(".attach-pills")).toHaveCount(0);
  await expect(page.locator(".attach-chip")).toHaveCount(0);
  await page.locator(".new-agent .composer").screenshot({ path: "artifacts/orb-inline-mentions.png" });
});

test("a path with spaces is quoted, and the payload keeps each identity", async ({ page }) => {
  const { posts, input } = await setup(page);

  await input.pressSequentially("read ");
  await mention(page, input, "my", /my notes\.md/);
  await input.pressSequentially("then ");
  await mention(page, input, "notes.md", /^notes\.md/);
  await input.press("Enter");

  await expect.poll(() => posts.length).toBe(1);
  // The trailing space after the last mention is trimmed off the sent prompt.
  expect(posts[0].prompt).toBe('read @"reference/my notes.md" then @notes.md');
  expect(posts[0].attachments).toEqual([
    { kind: "file", path: "reference/my notes.md" },
    { kind: "file", path: "notes.md" },
  ]);
});

test("deleting a mention detaches it; retyping attaches it again", async ({ page }) => {
  const { posts, input } = await setup(page);

  await input.pressSequentially("look at ");
  await mention(page, input, "notes.md", /^notes\.md/);
  await mention(page, input, "reference/spec", /reference\/spec\.md/);

  // Backspace over the last mention, exactly as a user would.
  const written = await input.inputValue();
  for (let i = 0; i < "@reference/spec.md ".length; i++) await input.press("Backspace");
  await expect(input).toHaveValue(written.replace("@reference/spec.md ", ""));

  await input.press("Enter");
  await expect.poll(() => posts.length).toBe(1);
  // Wait for the launch to finish being refused, not just for the request to
  // arrive: the composer ignores a second Enter while one is still in flight.
  await expect(page.getByRole("alert")).toContainText("Runner admission unavailable");
  // The deleted one must not ride along invisibly.
  expect(posts[0].attachments).toEqual([{ kind: "file", path: "notes.md" }]);
  expect(posts[0].prompt).not.toContain("reference/spec.md");

  // Typing it back attaches it again — the text is the source of truth.
  // Escape dismisses the picker that `@…` opened, so Enter sends the draft
  // rather than choosing a suggestion.
  await input.pressSequentially("@reference/spec.md");
  await input.press("Escape");
  await input.press("Enter");
  await expect.poll(() => posts.length).toBe(2);
  expect(posts[1].attachments).toEqual([
    { kind: "file", path: "notes.md" },
    { kind: "file", path: "reference/spec.md" },
  ]);
});

test("a repeated mention is sent once, and a failed send keeps the draft", async ({ page }) => {
  const { posts, input } = await setup(page);
  await input.pressSequentially("check @notes.md twice: @notes.md");
  await input.press("Escape");
  await input.press("Enter");

  await expect.poll(() => posts.length).toBe(1);
  expect(posts[0].attachments).toEqual([{ kind: "file", path: "notes.md" }]);
  // The send was refused; the sentence and both references survive verbatim.
  await expect(page.getByRole("alert")).toContainText("Runner admission unavailable");
  await expect(input).toHaveValue("check @notes.md twice: @notes.md");
});

test("mentions survive goal mode and reach the goal prompt", async ({ page }) => {
  const { posts, input } = await setup(page);
  await input.pressSequentially("/goal audit ");
  await expect(page.locator(".composer .mode-chip")).toBeVisible();
  await mention(page, input, "notes.md", /^notes\.md/);
  await input.press("Enter");

  await expect.poll(() => posts.length).toBe(1);
  expect(posts[0].prompt).toBe("/goal audit @notes.md");
  expect(posts[0].attachments).toEqual([{ kind: "file", path: "notes.md" }]);
});

test("sending waits for the attachment catalog instead of silently dropping typed mentions", async ({ page }) => {
  let release!: () => void;
  const ready = new Promise<void>(resolve => { release = resolve; });
  const { posts, input } = await setup(page, ready);
  await input.fill("Use @notes.md please");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  await expect(page.locator(".optimistic-message")).toContainText("Use @notes.md please");
  expect(posts).toHaveLength(0);
  release();
  await expect.poll(() => posts.length).toBe(1);
  expect(posts[0].attachments).toEqual([{ kind: "file", path: "notes.md" }]);
});
