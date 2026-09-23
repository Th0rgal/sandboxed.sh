import {render} from "solid-js/web";
import {MdView} from "../src/Markdown";
import "../src/styles.css";
document.documentElement.dataset.theme="dark";
render(()=><main style={{padding:"40px","max-width":"800px"}}><MdView text={'```python\ndef hello():\n    return "hello"\n```\n\n```text\n'+ 'long line '.repeat(50)+'\nlast line\n```'}/></main>,document.getElementById('root')!);
