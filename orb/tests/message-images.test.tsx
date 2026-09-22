import {expect,it,vi,afterEach} from "vitest";
import {render,screen,fireEvent,cleanup} from "@solidjs/testing-library";
import {messageImages} from "../src/messageImages";
import {UserTurn} from "../src/Transcript";
afterEach(cleanup);
it("replaces image transport lines with ordered thumbnails while keeping other files",()=>{
 const text="Compare #1 and #2.\n\n[Uploaded: /workspace/first.png]\n\n[Uploaded: /workspace/second.jpg]\n\n[Uploaded: /workspace/report.pdf]";
 expect(messageImages(text)).toEqual({text:"Compare #1 and #2.\n\n[Uploaded: /workspace/report.pdf]",paths:["/workspace/first.png","/workspace/second.jpg"]});
 render(()=><UserTurn text={text}/>);
 expect(screen.getByRole('button',{name:'Image #1'})).toBeTruthy();
 expect(screen.getByRole('button',{name:'Image #2'})).toBeTruthy();
 expect(screen.queryByText(/first\.png/)).toBeNull();
});
it("leaves ordinary text untouched and preserves inline numbering",()=>{
 expect(messageImages('  ordinary\n\n\ntext  ').text).toBe('  ordinary\n\n\ntext  ');
 expect(messageImages('See [Uploaded: /tmp/a.png] here.').text).toBe('See #1 here.');
});
it("reusing an edited prompt keeps its image attachments",()=>{
 const reuse=vi.fn();
 render(()=><UserTurn text={'Original\n\n[Uploaded: /tmp/a.png]'} onReuse={reuse}/>);
 fireEvent.click(screen.getByRole('button',{name:'Edit prompt'}));
 const editor=screen.getByRole('textbox');
 expect((editor as HTMLTextAreaElement).value).toBe('Original');
 fireEvent.input(editor,{target:{value:'Revised'}});
 fireEvent.click(screen.getByRole('button',{name:'Use as follow-up'}));
 expect(reuse).toHaveBeenCalledWith('Revised\n\n[Uploaded: /tmp/a.png]');
});
