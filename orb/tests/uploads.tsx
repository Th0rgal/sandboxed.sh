import { render } from "solid-js/web";
import { createSignal } from "solid-js";
import { Composer } from "../src/App";
import { setConnection } from "../src/api";
import "../src/styles.css";
setConnection(location.origin, "test-only");
document.documentElement.dataset.theme = "dark";
function Harness() {
 const [target,setTarget] = createSignal("core"); const [sent,setSent] = createSignal("");
 return <main style={{padding:"80px",background:"var(--bg)","min-height":"100vh"}}>
  <select aria-label="Machine" value={target()} onChange={e=>setTarget(e.currentTarget.value)}><option value="core">Core</option><option value="ashur">Ashur</option></select>
  <Composer placeholder="Describe a task" busy={false} tall picker={false} uploadTarget={target()} onStop={()=>{}} onSend={text=>{setSent(text);}} />
  <pre aria-label="Sent prompt">{sent()}</pre>
 </main>;
}
render(()=><Harness/>,document.getElementById("root")!);
