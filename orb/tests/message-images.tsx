import {render} from "solid-js/web";
import {FilePanelProvider} from "../src/FilePanel";
import {UserTurn} from "../src/Transcript";
import "../src/styles.css";
document.documentElement.dataset.theme="dark";
render(()=><FilePanelProvider scope={{mission:{id:"images",project:"test",status:"awaiting_user",title:"Images",history:[],created_at:"",updated_at:""}}}>
 <main style={{padding:"32px",width:"760px"}}><UserTurn text={"Compare #1 and #2 with the audit.\n\n[Uploaded: /workspace/.paloma/images/first.png]\n\n[Uploaded: /workspace/.paloma/images/second.png]"}/></main>
</FilePanelProvider>,document.getElementById("root")!);
