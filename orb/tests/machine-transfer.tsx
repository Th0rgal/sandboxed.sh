import { render } from "solid-js/web";
import { Show, createSignal } from "solid-js";
import { ChangeMachine } from "../src/ChangeMachine";
import "../src/styles.css";
import type { Mission } from "../src/api";
const mission: Mission = { id: "test", title: "Conversation", status: "awaiting_user", history: [], created_at: "", updated_at: "", backend: "codex", model_override: "model" };
localStorage.setItem("orb.apiUrl", location.origin);
function Fixture() {
  const [open, setOpen] = createSignal(false);
  const [location, setLocation] = createSignal("Core");
  return <div style={{ padding: "40px", "padding-top": "600px" }}><div class="fork-anchor"><button onClick={() => setOpen(!open())}>Change machine: {location()}</button><Show when={open()}><ChangeMachine mission={mission} choices={[{ backend: { id: "codex", name: "Codex" }, models: [{ value: "model", label: "GPT-6 Astra" }] }]} onClose={() => setOpen(false)} onMoved={() => setLocation("Spark")} /></Show></div></div>;
}
render(() => <Fixture />, document.getElementById("app")!);
