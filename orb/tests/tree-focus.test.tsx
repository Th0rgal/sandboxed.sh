import { render, fireEvent } from "@solidjs/testing-library";
import { expect, it, vi } from "vitest";
import { SidebarTree } from "../src/Tree";

it("focuses a clicked row so cut shortcuts bubble from that row on WebKit", () => {
  const cut = vi.fn();
  const ui = render(() => <div onKeyDown={e => {
    if (e.metaKey && e.key === "x") cut((e.target as HTMLElement).closest<HTMLElement>(".tree-entry")?.dataset.treeId);
  }}><SidebarTree nodes={[{ id: "mission", data: "Mission" }]} label="Projects" selected="mission"
    render={row => <button><span>{row.data}</span></button>} /></div>);
  fireEvent.click(ui.getByText("Mission"));
  expect(document.activeElement).toBe(ui.getByRole("button"));
  fireEvent.keyDown(document.activeElement!, { key: "x", metaKey: true });
  expect(cut).toHaveBeenCalledWith("mission");
  ui.unmount();
});
