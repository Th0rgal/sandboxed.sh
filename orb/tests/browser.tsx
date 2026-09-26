import { render } from "solid-js/web";
import { createSignal, Show } from "solid-js";
import { ControllerView } from "../src/Controller";
import { LiveProjectsSection } from "../src/ProjectFiles";
import "../src/styles.css";
localStorage.setItem("orb.apiUrl", window.location.origin);
localStorage.setItem("orb.jwt", "local-browser-test");
// api.ts creates its connected signal before the assignment above.
import { clearConnection, setConnection } from "../src/api";
setConnection(window.location.origin, "local-browser-test");
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") ?? "dark";
function Harness() {
  const [selected, setSelected] = createSignal<string | null>(null);
  const [visible, setVisible] = createSignal(true);
  return <div style={{ display: "flex", height: "100vh", background: "var(--bg)" }}>
    <aside style={{ width: "260px", padding: "20px 10px", "flex-shrink": 0 }}>
      <LiveProjectsSection selected={selected} open={setSelected} onNewAgent={() => {}} onNewProject={() => {}} />
      <div class="harness-controls"><button onClick={clearConnection}>Disconnect backend</button><button onClick={() => setConnection(window.location.origin, "local-browser-test")}>Reconnect backend</button><button class="s-btn" onClick={() => setVisible(!visible())}>Toggle view</button>
      <a href="#notes">Project link</a><input disabled aria-label="Disabled field" /></div>
    </aside>
    <main style={{ flex: 1, "min-width": 0, overflow: "auto" }}>
      <Show when={selected() && visible()}><ControllerView slug="notes" id={selected()!.split(":")[2]} /></Show>
    </main>
  </div>;
}
render(() => <Harness />, document.getElementById("root")!);
