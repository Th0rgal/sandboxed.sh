import { render } from "solid-js/web";
import { createSignal, Show } from "solid-js";
import { RoutingSettings } from "../src/RoutingSettings";
import { setConnection } from "../src/api";
import "../src/styles.css";
setConnection("https://routing.test", "fixture");
document.documentElement.dataset.theme = "dark";
function Harness() {
 const [open, setOpen] = createSignal(true);
 return <div class="app" style={{"--sb-w":"195px"}}><aside class="sidebar"><button onClick={() => setOpen(!open())}>Toggle routing</button></aside><main class="main"><div class="toolbar"/><Show when={open()}><RoutingSettings onOpenClient={() => {}} /></Show></main></div>;
}
render(() => <Harness />, document.getElementById("root")!);
