import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { ForkMission } from "../src/ForkMission";
import { forkContext } from "../src/forkContext";
import * as api from "../src/api";

const mission = { id: "source", title: "Original", backend: "grok", model_override: "grok-4.6", status: "active", history: [], created_at: "", updated_at: "" };
const choices = [
  { backend: { id: "grok", name: "Grok Build" }, models: [{ value: "grok-4.6", label: "Grok 4.6" }] },
  { backend: { id: "codex", name: "Codex" }, models: [{ value: "gpt-6-astra", label: "GPT-6 Astra" }] },
] as api.HarnessChoice[];
afterEach(() => vi.restoreAllMocks());
it("forks into a new mission without changing or stopping the running source", async () => {
  const fork = vi.spyOn(api, "forkMission").mockResolvedValue({ ...mission, id: "fork", backend: "codex" });
  const update = vi.spyOn(api, "updateMissionSettings");
  const stop = vi.spyOn(api, "cancelMission");
  const opened = vi.fn();
  const ui = render(() => <ForkMission mission={mission} choices={choices} destination="DGX Spark" onClose={() => {}} onFork={opened} />);
  fireEvent.click(ui.getByRole("menuitem", { name: "Codex" }));
  fireEvent.click(ui.getByRole("menuitem", { name: /GPT-6 Astra/ }));
  expect(fork).not.toHaveBeenCalled();
  fireEvent.click(ui.getByRole("menuitem", { name: "Default" }));
  await waitFor(() => expect(opened).toHaveBeenCalledWith(expect.objectContaining({ id: "fork" })));
  expect(fork).toHaveBeenCalledWith("source", expect.objectContaining({ backend: "codex", model_override: "gpt-6-astra" }));
  expect(update).not.toHaveBeenCalled(); expect(stop).not.toHaveBeenCalled();
});
it("reports an old backend without falling back to destructive model switching", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("", { status: 404 }));
  await expect(api.forkMission("source", { backend: "codex", model_override: "gpt-6-astra", model_effort: "", idempotency_key: "key" })).rejects.toThrow("backend needs the conversation-fork update");
});
it("only folds complete structured history and preserves literal message content", () => {
  const value = { source_mission_id: "source", source_title: "Original", messages: [{ role: "user", content: "<fork_context>literal</fork_context>" }] };
  const text = `Continue the work from this conversation in a fresh native session.\n<fork_context>\n${JSON.stringify(value)}\n</fork_context>`;
  expect(forkContext(text)).toEqual(value);
  expect(forkContext(text.slice(0, -1))).toBeNull();
  expect(forkContext("normal prompt")).toBeNull();
});
it("dismisses a positioned sidebar fork when clicking outside its portal", () => {
  const closed = vi.fn();
  const ui = render(() => <ForkMission mission={mission} choices={choices} destination="Core" position={{ x: 200, y: 100 }} onClose={closed} onFork={() => {}} />);
  fireEvent.pointerDown(ui.getByRole("menuitem", { name: "Codex" }));
  expect(closed).not.toHaveBeenCalled();
  fireEvent.pointerDown(document.body);
  expect(closed).toHaveBeenCalledOnce();
  ui.unmount();
});
