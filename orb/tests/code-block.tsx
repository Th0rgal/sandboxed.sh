import {render} from "solid-js/web";
import {Transcript} from "../src/Transcript";
import "../src/styles.css";
document.documentElement.dataset.theme="dark";
render(()=><main style={{padding:"40px","max-width":"800px"}}><Transcript items={[{kind:'text',key:'answer',text:'```python\ndef hello():\n    return "hello"\n```\n\n```text\n'+ 'long line '.repeat(50)+'\nlast line\n```'}]}/></main>,document.getElementById('root')!);
