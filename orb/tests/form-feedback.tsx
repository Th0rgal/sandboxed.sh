import {render} from 'solid-js/web';
import {Composer} from '../src/App';
import {CronForm} from '../src/ControllerSettings';
import {getProjectCronFromJob} from '../src/cronSchema';
import fixtures from './fixtures/hermes-jobs.json';
import '../src/styles.css';
document.documentElement.dataset.theme='dark';
const view=getProjectCronFromJob('notes',fixtures.hourly);
render(()=><main style={{padding:'30px','max-width':'800px',margin:'auto',height:'100vh',overflow:'auto'}}>{location.search.includes('cron')?<CronForm draftKey="feedback" view={view} save={async()=>view} onSaved={()=>{}}/>:<Composer placeholder="Send follow-up" busy={false} onSend={()=>new Promise<boolean>(resolve=>{(window as any).finish=resolve;})} onStop={()=>{}}/>}</main>,document.getElementById('root')!);
