import { describe, expect, it } from "vitest";
import { missionMachine, placeRowTip, rowDetail } from "../src/ProjectFiles";

describe("sidebar row details stay factual", () => {
  it("uses remote node, then workspace, and never invents a repo or branch", () => {
    expect(missionMachine({ remote_node_id: "dgx-spark", workspace_name: "host" })).toBe("DGX Spark");
    expect(missionMachine({ workspace_name: "verity" })).toBe("verity");
    expect(missionMachine({})).toBeUndefined();
    expect(rowDetail("Short")).toEqual({ title: "Short", meta: [] });
    expect(rowDetail("Check remote startup without losing this draft", ["DGX Spark"]))
      .toEqual({ title: "Check remote startup without losing this draft", meta: ["DGX Spark"] });
    expect(rowDetail("Short", [undefined, "Core"])).toEqual({ title: "Short", meta: ["Core"] });
  });
  it("places the card beside the row when there is room and clamps to the viewport", () => {
    expect(placeRowTip({ top: 80, left: 8, right: 220, bottom: 110 }, { width: 240, height: 44 }, { width: 1100, height: 900 }))
      .toEqual({ x: 188, y: 80 });
    const clamped = placeRowTip({ top: 860, left: 900, right: 1090, bottom: 890 }, { width: 240, height: 80 }, { width: 1100, height: 900 });
    expect(clamped.x).toBeLessThanOrEqual(1100 - 240 - 8);
    expect(clamped.y).toBeLessThanOrEqual(900 - 80 - 8);
    expect(clamped.x).toBeGreaterThanOrEqual(8);
    expect(clamped.y).toBeGreaterThanOrEqual(8);
  });
});
