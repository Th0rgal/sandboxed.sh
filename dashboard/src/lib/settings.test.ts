import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import {
  readSavedSettings,
  writeSavedSettings,
  getRuntimeApiBase,
} from "./settings";

describe("readSavedSettings", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("returns empty object when nothing is saved", () => {
    expect(readSavedSettings()).toEqual({});
  });

  it("reads a valid apiUrl from localStorage", () => {
    localStorage.setItem("settings", JSON.stringify({ apiUrl: "http://myhost:4000" }));
    expect(readSavedSettings()).toEqual({ apiUrl: "http://myhost:4000" });
  });

  it("ignores non-string apiUrl values", () => {
    localStorage.setItem("settings", JSON.stringify({ apiUrl: 42 }));
    expect(readSavedSettings()).toEqual({});
  });

  it("returns empty object for invalid JSON", () => {
    localStorage.setItem("settings", "not-json");
    expect(readSavedSettings()).toEqual({});
  });
});

describe("writeSavedSettings", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("persists apiUrl to localStorage", () => {
    writeSavedSettings({ apiUrl: "http://test:5000" });
    const raw = localStorage.getItem("settings");
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw!)).toEqual({ apiUrl: "http://test:5000" });
  });
});

describe("getRuntimeApiBase", () => {
  const originalEnv = process.env.NEXT_PUBLIC_API_URL;

  beforeEach(() => {
    localStorage.clear();
    delete process.env.NEXT_PUBLIC_API_URL;
  });

  afterEach(() => {
    if (originalEnv !== undefined) {
      process.env.NEXT_PUBLIC_API_URL = originalEnv;
    } else {
      delete process.env.NEXT_PUBLIC_API_URL;
    }
  });

  it("returns saved setting when present", () => {
    localStorage.setItem(
      "settings",
      JSON.stringify({ apiUrl: "http://custom:9999/" })
    );
    expect(getRuntimeApiBase()).toBe("http://custom:9999");
  });

  it("returns env var when no saved setting", () => {
    process.env.NEXT_PUBLIC_API_URL = "http://env-host:8080";
    expect(getRuntimeApiBase()).toBe("http://env-host:8080");
  });

  it("falls back to window.location.origin when no saved setting or env var", () => {
    // The key behavior: uses window.location.origin instead of hardcoding :3000.
    // In jsdom the origin is 'http://localhost' by default.
    const result = getRuntimeApiBase();
    expect(result).toBe(window.location.origin);
  });

  it("strips trailing slash from returned URL", () => {
    localStorage.setItem(
      "settings",
      JSON.stringify({ apiUrl: "http://host:3000/" })
    );
    expect(getRuntimeApiBase()).toBe("http://host:3000");
  });
});
