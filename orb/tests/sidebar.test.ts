import { describe, expect, it } from "vitest";
import { missionMachine, rowDetail } from "../src/ProjectFiles";

describe("sidebar row details stay factual", () => {
  it("uses remote node, then workspace, and never invents a repo or branch", () => {
    expect(missionMachine({ remote_node_id: "dgx-spark", workspace_name: "host" })).toBe("DGX Spark");
    expect(missionMachine({ workspace_name: "verity" })).toBe("verity");
    expect(missionMachine({})).toBeUndefined();
    expect(rowDetail("Short")).toBeUndefined();
    expect(rowDetail("Check remote startup without losing this draft", ["DGX Spark"]))
      .toBe("Check remote startup without losing this draft · DGX Spark");
    expect(rowDetail("Short", [undefined, "Core"])).toBe("Core");
  });
});
