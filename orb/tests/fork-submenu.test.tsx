import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { createSignal, Show } from "solid-js";
import { expect, it, vi } from "vitest";
import { PopupMenu } from "../src/Menu";

it("keeps the parent menu open on hover and treats submenu clicks as inside", async () => {
  const closed = vi.fn();
  const action = vi.fn();
  const ui = render(() => {
    const [submenu, setSubmenu] = createSignal(false);
    return <PopupMenu x={20} y={30} focus={false} onDismissSubmenu={() => setSubmenu(false)} onClose={closed} items={[
      { kind: "item", label: "Fork conversation", openOnHover: true, onClick: () => setSubmenu(true) },
      { kind: "item", label: "Copy mission ID", onClick: () => {} },
    ]}>
      <Show when={submenu()}><div role="menu"><button role="menuitem" onClick={action}>Codex</button></div></Show>
    </PopupMenu>;
  });
  fireEvent.mouseEnter(ui.getByRole("menuitem", { name: "Fork conversation" }));
  const model = await waitFor(() => ui.getByRole("menuitem", { name: "Codex" }));
  expect(ui.getByRole("menuitem", { name: "Copy mission ID" })).toBeTruthy();
  fireEvent.pointerDown(model);
  fireEvent.click(model);
  expect(action).toHaveBeenCalledOnce();
  expect(closed).not.toHaveBeenCalled();
  fireEvent.mouseEnter(ui.getByRole("menuitem", { name: "Copy mission ID" }));
  expect(ui.queryByRole("menuitem", { name: "Codex" })).toBeNull();
  expect(closed).not.toHaveBeenCalled();
  fireEvent.pointerDown(document.body);
  expect(closed).toHaveBeenCalledOnce();
  ui.unmount();
});
