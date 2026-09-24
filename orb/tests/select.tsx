import { render } from "solid-js/web";
import { createSignal } from "solid-js";
import { Select } from "../src/Select";
import { RoutingPicker } from "../src/RoutingPicker";
import "../src/styles.css";
document.documentElement.dataset.theme = "dark";
function Fixture() {
 const [value,setValue]=createSignal('off');
 const [model,setModel]=createSignal('grok-4.6');
 return <main style={{padding:'60px',background:'var(--bg)',height:'100vh'}}>
  <label>Open in <Select aria-label="Open in" value={value()} onChange={e=>setValue(e.currentTarget.value)}><option value="off">Off</option><option value="browser">Browser Tab</option><option value="disabled" disabled>Unavailable</option></Select></label>
  <button onClick={()=>setValue('off')}>Reset</button>
  <output>{value()}</output>
  <div style={{width:'260px',margin:'30px 0'}}><RoutingPicker label="Model" value={model()} onInput={setModel} options={[{id:'grok-4.6',name:'Grok 4.6'},{id:'grok-4.6-latest',name:'Grok 4.6 (Latest)'}]}/></div>
 </main>;
}
render(()=> <Fixture/>,document.getElementById('root')!);
