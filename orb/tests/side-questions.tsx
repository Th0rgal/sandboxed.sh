import {render} from 'solid-js/web';
import {createSignal} from 'solid-js';
import {SideQuestions,type SideQuestionsHandle} from '../src/SideQuestionPanel';
import {Composer} from '../src/App';
import '../src/styles.css';
function Fixture(){let handle!:SideQuestionsHandle;const [revision,setRevision]=createSignal<{text:string;append:boolean}>();
 return <main style={{padding:'24px','max-width':'760px',margin:'auto'}}><p>The build is running. I’ll report the results when it finishes.</p><p class="dim">Agent is working</p><div style={{'margin-top':'80px'}}><SideQuestions mission="browser-fixture" items={[{kind:'text',key:'1',text:'The build is running.',live:false}]} ref={h=>handle=h} onTransfer={text=>setRevision({text,append:true})}/><Composer placeholder="Send follow-up" revision={revision()} busy onSend={()=>{throw new Error('Side question reached agent!');}} onStop={()=>{throw new Error('Agent was stopped!');}} onBtw={q=>handle.ask(q)} onOpenBtw={()=>handle.open()}/></div></main>;
}render(()=><Fixture/>,document.getElementById('root')!);
