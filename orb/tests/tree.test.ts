import { describe, it, expect } from "vitest";
import { visibleTree, type TreeNode } from "../src/treeModel";
const node = (id: string, children?: TreeNode<string>[], expanded = true): TreeNode<string> => ({ id, data: id, ...(children ? { children, expanded } : {}) });
describe("visible sidebar hierarchy", () => {
  it("terminates the project rail at Finished when its six missions are the final subtree", () => {
    const rows = visibleTree([node("test", [node("finished", Array.from({length:6}, (_,i)=>node(`mission-${i}`)))])]);
    expect(rows.map(r=>r.depth)).toEqual([0,1,2,2,2,2,2,2]);
    expect(rows[1]).toMatchObject({ following:false, connectsChildren:true, position:1, size:1 });
    expect(rows.slice(2).map(r=>r.continuations)).toEqual(Array(6).fill([]));
    expect(rows.at(-1)).toMatchObject({ following:false, parentId:"finished", position:6, size:6 });
    expect(rows.slice(2,-1).every(r=>r.following)).toBe(true);
  });
  it("continues exactly ancestors with following siblings at arbitrary depth", () => {
    const rows = visibleTree([node("p", [node("a", [node("b", [node("c", [node("d")])]),node("a-tail")]),node("p-tail")])]);
    expect(rows.find(r=>r.id==="d")).toMatchObject({depth:4,continuations:[0,1],following:false});
    expect(rows.find(r=>r.id==="a-tail")).toMatchObject({depth:2,continuations:[0],following:false});
    expect(rows.at(-1)).toMatchObject({id:"p-tail",continuations:[],following:false});
  });
  it("uses visible children for collapsed, loading and empty branches and preserves IDs", () => {
    const rows = visibleTree([node("p", [node("closed",[node("hidden")],false),node("loading",[node("loading-note")]),node("empty",[])])]);
    expect(rows.map(r=>r.id)).toEqual(["p","closed","loading","loading-note","empty"]);
    expect(rows.find(r=>r.id==="closed")).toMatchObject({expanded:false,connectsChildren:false});
    expect(rows.at(-1)).toMatchObject({connectsChildren:false,following:false});
  });
});
