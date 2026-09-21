import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  DEFAULT_EFFORT_LABEL,
  EFFORT_BY_HARNESS,
  EFFORT_LADDER,
  effortLabel,
  harnessSupportsEffort,
  normalizeEffort,
  supportedEfforts,
} from "../src/effort";

// vitest runs with `orb/` as the cwd; the core lives one level up.
const CONTROL_RS = resolve(process.cwd(), "../src/api/control/mod.rs");

/**
 * Re-derive the accepted ladder from the core's own gate,
 * `normalize_model_effort_for_backend`, so Orb's table cannot drift from the
 * server that validates it.
 */
function effortsFromCore(): Record<string, string[]> {
  const source = readFileSync(CONTROL_RS, "utf8");
  const fn = source.slice(source.indexOf("fn normalize_model_effort_for_backend"));
  const body = fn.slice(0, fn.indexOf("\n}\n"));
  const out: Record<string, string[]> = {};
  for (const [, backend, arms] of body.matchAll(/\(Some\("(\w+)"\),\s*([^)]+?)\)\s*=>/g)) {
    out[backend] = [...arms.matchAll(/"(\w+)"/g)].map((m) => m[1]);
  }
  return out;
}

describe("effort options come from the core's own gate", () => {
  it("matches normalize_model_effort_for_backend exactly", () => {
    const core = effortsFromCore();
    expect(Object.keys(core).sort()).toEqual(["claudecode", "codex"]);
    for (const [backend, ladder] of Object.entries(core)) {
      expect(supportedEfforts(backend)).toEqual(ladder);
    }
    expect(Object.keys(EFFORT_BY_HARNESS).sort()).toEqual(Object.keys(core).sort());
  });

  it("never offers 'ultra', which the help string names but the gate rejects", () => {
    // supported_model_efforts_for_backend lists it for codex; the gate does not
    // accept it, so offering it would produce a 400 on create.
    expect(EFFORT_LADDER).not.toContain("ultra");
    expect(normalizeEffort("ultra", "codex")).toBeNull();
  });

  it("offers nothing for harnesses the core forces to null", () => {
    for (const backend of ["opencode", "grok", "gemini", "chatgpt_ui", "", null, undefined]) {
      expect(supportedEfforts(backend)).toEqual([]);
      expect(harnessSupportsEffort(backend)).toBe(false);
      expect(normalizeEffort("high", backend)).toBeNull();
    }
  });
});

describe("normalizeEffort keeps an invalid selection out of a payload", () => {
  it("keeps a supported level and drops everything else", () => {
    expect(normalizeEffort("high", "codex")).toBe("high");
    expect(normalizeEffort("MAX", "claudecode")).toBe("max");
    expect(normalizeEffort("  xhigh  ", "codex")).toBe("xhigh");
    expect(normalizeEffort("turbo", "codex")).toBeNull();
    expect(normalizeEffort("", "codex")).toBeNull();
    expect(normalizeEffort(null, "codex")).toBeNull();
  });

  it("drops a level when the harness changes to one that cannot take it", () => {
    // Codex → OpenCode: the stored effort must not ride along.
    expect(normalizeEffort("max", "opencode")).toBeNull();
  });

  it("labels an unset effort as the backend default, never a guessed level", () => {
    expect(effortLabel(null)).toBe(DEFAULT_EFFORT_LABEL);
    expect(effortLabel("")).toBe(DEFAULT_EFFORT_LABEL);
    expect(effortLabel("nonsense")).toBe(DEFAULT_EFFORT_LABEL);
    expect(effortLabel("xhigh")).toBe("XHigh");
  });
});
