import { describe, it, expect, beforeEach, afterEach } from "vitest";
import {
  readSavedSettings,
  writeSavedSettings,
  getRuntimeApiBase,
  inferHostedApiBase,
  inferLocalApiBase,
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

  it("does not preserve a saved local frontend origin as the API URL", () => {
    localStorage.setItem(
      "settings",
      JSON.stringify({ apiUrl: window.location.origin })
    );
    expect(getRuntimeApiBase()).toBe("http://localhost:3000");
  });

  it("returns env var when no saved setting", () => {
    process.env.NEXT_PUBLIC_API_URL = "http://env-host:8080";
    expect(getRuntimeApiBase()).toBe("http://env-host:8080");
  });

  it("maps a local browser origin to the default backend port", () => {
    window.history.replaceState({}, "", "/control");
    expect(getRuntimeApiBase()).toBe("http://localhost:3000");
  });

  it("keeps same-origin when the local page already runs on the backend port", () => {
    window.history.replaceState({}, "", "http://localhost:3000/control");
    expect(getRuntimeApiBase()).toBe("http://localhost:3000");
  });

  it("strips trailing slash from returned URL", () => {
    localStorage.setItem(
      "settings",
      JSON.stringify({ apiUrl: "http://host:3000/" })
    );
    expect(getRuntimeApiBase()).toBe("http://host:3000");
  });
});

describe("inferHostedApiBase", () => {
  it("maps the production dashboard host to the production backend", () => {
    expect(inferHostedApiBase("agent.thomas.md")).toBe("https://agent-backend.thomas.md");
  });

  it("returns null for unknown hosts", () => {
    expect(inferHostedApiBase("example.com")).toBeNull();
  });
});

describe("inferLocalApiBase", () => {
  it("maps localhost frontend ports to :3000", () => {
    expect(inferLocalApiBase(new URL("http://localhost:3001/control") as unknown as Location)).toBe(
      "http://localhost:3000"
    );
  });

  it("does not override non-local hosts", () => {
    expect(inferLocalApiBase(new URL("https://example.com/control") as unknown as Location)).toBeNull();
  });
});
