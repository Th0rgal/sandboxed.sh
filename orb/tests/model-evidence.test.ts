import { describe, expect, it } from "vitest";
import { modelEvidence, type ModelDiscovery } from "../src/routingApi";

describe("model discovery evidence", () => {
  const observation = (status: string): ModelDiscovery => ({ connections: [{
    provider_id: "zai", access_profile: "coding", status,
    source: status === "discovered" ? "discovery" : "stale_discovery",
    checked_at: "2026-09-24T07:00:00Z",
    last_success: { observed_at: "2026-09-24T07:00:00Z", completeness: "partial", models: [{ id: "glm-5.3" }] },
  }] });
  it("distinguishes current discovery, stale evidence and unverified fallbacks", () => {
    expect(modelEvidence(observation("discovered"), "zai", "glm-5.3")).toBe("Listed by provider");
    expect(modelEvidence(observation("error"), "zai", "glm-5.3")).toBe("Previously listed · refresh failed");
    expect(modelEvidence(observation("discovered"), "zai", "unknown")).toBe("Fallback / unverified");
    expect(modelEvidence(observation("discovered"), "other", "glm-5.3")).toBe("Fallback / unverified");
    expect(modelEvidence(undefined, "zai", "glm-5.3")).toBe("Fallback / unverified");
  });
});
