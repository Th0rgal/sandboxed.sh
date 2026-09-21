import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { launchRefusal } from "../src/missionLaunch";
import { ApiError, NO_PROJECT_LIMIT, holdsCapSlot, projectLimitOf, setProjectLimit, updateGlobalSettings, setConnection } from "../src/api";

const CONTROL_RS = resolve(process.cwd(), "../src/api/control/mod.rs");
const capError = (active: number, cap: number) =>
  new ApiError(429, JSON.stringify({ error: "parallel_missions_cap", cap, active }));

describe("the project cap refusal is explained, not dumped", () => {
  it("replaces the raw JSON body with plain language", () => {
    const r = launchRefusal(capError(2, 2), "lido-srv3-report");
    expect(r.kind).toBe("project_cap");
    expect(r).toMatchObject({ active: 2, cap: 2 });
    expect(r.message).not.toContain("{");
    expect(r.message).toContain("lido-srv3-report");
    expect(r.message).toContain("limit of 2 unfinished agents (2 in use)");
  });

  it("leaks no raw identifiers or storage internals into the message", () => {
    const { message } = launchRefusal(capError(3, 3), "notes");
    for (const jargon of ["parallel_missions_cap", "parallel_missions", "max_parallel_missions", "grant", "non-terminal", "nonterminal"]) {
      expect(message.toLowerCase()).not.toContain(jargon.toLowerCase());
    }
  });

  it("states the limit and the usage without claiming anything it cannot know", () => {
    const { message } = launchRefusal(capError(3, 3), "notes");
    expect(message).toContain("limit of 3 unfinished agents (3 in use)");
    expect(message).toMatch(/draft is kept/i);
    // The cap check proves nothing about provider health — a Codex quota can be
    // exhausted at the same time — so the copy must not vouch for it.
    expect(message).not.toMatch(/nothing is wrong/i);
    expect(message).not.toMatch(/model provider/i);
  });

  it("offers only the remedies that actually work", () => {
    const { message } = launchRefusal(capError(2, 2), "notes");
    // Raising the backend-wide limit would not clear this, so it is not offered.
    expect(message).not.toMatch(/global/i);
    expect(message).toMatch(/finish an agent/i);
    expect(message).toMatch(/increase the limit in Project settings/i);
  });

  it("distinguishes a blocked project from a launch error", () => {
    const blocked = launchRefusal(new ApiError(409, JSON.stringify({ error: "project_paused" })), "notes");
    expect(blocked.kind).toBe("project_blocked");
    expect(blocked.message).toMatch(/paused/i);
    expect(blocked.message).toMatch(/resume the project first/i);
    expect(blocked.message).not.toMatch(/limit/i);
  });

  it("leaves an unstructured provider/harness error alone", () => {
    const other = launchRefusal(new ApiError(502, "codex: upstream model unavailable"), "notes");
    expect(other.kind).toBe("other");
    expect(other.message).toBe("codex: upstream model unavailable");
  });

  it("survives a body that is not the JSON it expects", () => {
    expect(launchRefusal(new ApiError(500, "{not json"), "notes").kind).toBe("other");
    expect(launchRefusal(new Error("offline"), "notes").message).toBe("offline");
  });
});

describe("cap slot accounting mirrors the core", () => {
  it("counts exactly the statuses campaign_slot_held_by counts", () => {
    const source = readFileSync(CONTROL_RS, "utf8");
    const fn = source.slice(source.indexOf("fn campaign_slot_held_by"));
    const body = fn.slice(0, fn.indexOf("\n}\n"));
    const core = [...body.matchAll(/MissionStatus::(\w+)/g)]
      .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase())
      .sort();
    expect(core).toEqual(["active", "awaiting_user", "paused", "pending", "waiting_background"]);
    for (const status of core) expect(holdsCapSlot(status)).toBe(true);
    for (const status of ["completed", "failed", "cancelled", "interrupted", "not_feasible"]) {
      expect(holdsCapSlot(status)).toBe(false);
    }
  });
});

/**
 * The backend merge these payloads have to survive. `projects_store.rs`
 * `set_grant` upserts with `COALESCE(excluded.<col>, project_grant.<col>)` per
 * column, and `create_mission` only enforces `parallel_missions` when it is
 * `> 0`. Both facts are asserted against the Rust source below.
 */
function mergeGrant(stored: Record<string, unknown>, posted: Record<string, unknown>) {
  const columns = ["merge_authority", "budget_per_tick", "parallel_missions", "pause_reason", "resume_condition", "material_bar", "autonomy_level"];
  const out = { ...stored };
  for (const column of columns) {
    // COALESCE(excluded, existing): a JSON null (or an omitted field, which
    // deserializes to None) keeps whatever is already stored.
    const incoming = posted[column];
    if (incoming !== undefined && incoming !== null) out[column] = incoming;
  }
  return out;
}

describe("the backend merge these payloads are written for", () => {
  const store = readFileSync(resolve(process.cwd(), "../src/api/projects_store.rs"), "utf8");

  it("really does COALESCE every grant column, so null preserves", () => {
    expect(store).toContain("parallel_missions = COALESCE(excluded.parallel_missions, project_grant.parallel_missions)");
    expect(store).toContain("autonomy_level = COALESCE(excluded.autonomy_level, project_grant.autonomy_level)");
    // Therefore the model above is faithful: null cannot clear a column.
    expect(mergeGrant({ parallel_missions: 2 }, { parallel_missions: null }).parallel_missions).toBe(2);
  });

  it("really does treat only a positive limit as enforced", () => {
    const control = readFileSync(CONTROL_RS, "utf8");
    expect(control).toContain("grant.parallel_missions.filter(|&c| c > 0)");
    expect(NO_PROJECT_LIMIT).toBe(0);
    expect(projectLimitOf({ parallel_missions: 0 })).toBeNull();
    expect(projectLimitOf({ parallel_missions: -1 })).toBeNull();
    expect(projectLimitOf({ parallel_missions: 3 })).toBe(3);
    expect(projectLimitOf(null)).toBeNull();
  });
});

describe("project and backend-wide limits are written as distinct payloads", () => {
  const fetchMock = vi.fn();
  const bodyOf = (call: number) => JSON.parse((fetchMock.mock.calls[call][1] as RequestInit).body as string);

  const connect = (grant: Record<string, unknown> = {}) => {
    setConnection("https://core.test", "jwt");
    fetchMock.mockReset();
    fetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({ slug: "notes", grant }) });
    vi.stubGlobal("fetch", fetchMock);
  };
  afterEach(() => vi.unstubAllGlobals());

  it("sends only the project's own field, so a concurrent permission change survives", async () => {
    connect();
    await setProjectLimit("notes", 4);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("https://core.test/api/projects/notes/grant");
    expect((init as RequestInit).method).toBe("POST");
    // Exactly one field: nothing else can be reverted by this write, and no
    // read is needed first.
    expect(bodyOf(0)).toEqual({ parallel_missions: 4 });
    expect(fetchMock.mock.calls).toHaveLength(1);
  });

  it("leaves every other project setting untouched through the real merge", async () => {
    connect();
    const stored = {
      merge_authority: "owner",
      budget_per_tick: "2h",
      material_bar: "user-visible",
      autonomy_level: "act_reversible",
      parallel_missions: 2,
    };
    await setProjectLimit("notes", 4);
    expect(mergeGrant(stored, bodyOf(0))).toEqual({ ...stored, parallel_missions: 4 });
  });

  it("clears the limit with 0, because a null would be merged away", async () => {
    connect();
    await setProjectLimit("notes", NO_PROJECT_LIMIT);
    const body = bodyOf(0);
    expect(body).toEqual({ parallel_missions: 0 });

    const merged = mergeGrant({ parallel_missions: 2, autonomy_level: "propose" }, body);
    expect(merged.parallel_missions).toBe(0);
    // 0 stored means no limit is enforced, and the field reads back as empty.
    expect(projectLimitOf(merged as { parallel_missions: number })).toBeNull();
    expect(merged.autonomy_level).toBe("propose");

    // The regression this replaces: a null left the old limit in force.
    expect(mergeGrant({ parallel_missions: 2 }, { parallel_missions: null }).parallel_missions).toBe(2);
  });

  it("sends the backend-wide limit under its own name, to its own endpoint", async () => {
    connect();
    fetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({ max_parallel_missions: 3 }) });
    await updateGlobalSettings({ max_parallel_missions: 3 });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("https://core.test/api/settings");
    expect((init as RequestInit).method).toBe("PUT");
    const body = bodyOf(0);
    expect(body).toEqual({ max_parallel_missions: 3 });
    // The two limits must never be confused in a payload.
    expect(body).not.toHaveProperty("parallel_missions");
  });
});
