import {render} from 'solid-js/web';
import {ContextHistory} from '../src/ContextHistory';
import '../src/styles.css';
document.documentElement.dataset.theme='dark';
render(()=><main style={{padding:'48px',height:'100vh',background:'var(--bg)',color:'var(--fg)'}}><div class="pf-bar"><span>Research / notes.md</span><span class="dlg-spacer"/><ContextHistory slug="notes" path="notes.md" onRestore={()=>{}}/></div><p>Shared project context</p></main>,document.getElementById('root')!);
