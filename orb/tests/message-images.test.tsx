import {FileReferenceContext} from "../src/fileReferenceContext";
import {expect,it,vi,afterEach} from "vitest";
import {render,screen,fireEvent,cleanup,waitFor} from "@solidjs/testing-library";
import {messageImages} from "../src/messageImages";
import {UserTurn} from "../src/Transcript";
afterEach(cleanup);
it("replaces image transport lines with ordered thumbnails while keeping other files",()=>{
 const text="Compare #1 and #2.\n\n[Uploaded: /workspace/first.png]\n\n[Uploaded: /workspace/second.jpg]\n\n[Uploaded: /workspace/report.pdf]";
 expect(messageImages(text)).toEqual({text:"Compare #1 and #2.\n\n[Uploaded: /workspace/report.pdf]",paths:["/workspace/first.png","/workspace/second.jpg"],references:[1,2]});
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
 const reuse=vi.fn().mockResolvedValue(true);
 render(()=><UserTurn text={'Original\n\n[Uploaded: /tmp/a.png]'} onSend={reuse}/>);
 fireEvent.click(screen.getByRole('button',{name:'Edit prompt'}));
 const editor=screen.getByRole('textbox');
 expect((editor as HTMLTextAreaElement).value).toBe('Original');
 fireEvent.input(editor,{target:{value:'Revised'}});
 fireEvent.click(screen.getByRole('button',{name:'Send follow-up'}));
 expect(reuse).toHaveBeenCalledWith('Revised\n\n[Image #1] [Uploaded: /tmp/a.png]');
});

it("removes numbered transport metadata while preserving the pasted reference",()=>{
 const result=messageImages('Before\n[Image #3]\nAfter\n\n[Image #3] [Uploaded: /tmp/a.png]');
 expect(result).toEqual({text:'Before\n[Image #3]\nAfter',paths:['/tmp/a.png'],references:[3]});
 expect(messageImages('See [Image #3] [Uploaded: /tmp/a.png] here.').text).toBe('See [Image #3] here.');
});
it("decodes legacy optimistic image markers without leaking base64 into text",()=>{
 const data='data:image/png;base64,aGVsbG8=';
 const result=messageImages(`Look [Image #1]\n\n[Image #1] [Uploaded: ${data}]`);
 expect(result).toEqual({text:'Look [Image #1]',paths:[data],references:[1]});
});
it("does not interpret other file types or remote URLs as image uploads",()=>{
 for(const value of ['[Uploaded: https://example.com/a.png]','[Uploaded: /tmp/a.pdf]','[Uploaded: data:text/html;base64,aGVsbG8=]']) expect(messageImages(value).text).toBe(value);
});
it("renders structured pending images without embedding bytes in the message body",async()=>{
 const dataUrl='data:image/png;base64,aGVsbG8=';
 render(()=><UserTurn text="Inspect [Image #3]" images={[{id:'x',name:'x.png',type:'image/png',reference:3,dataUrl}]}/>);
 await waitFor(()=>expect(screen.getByAltText('Image #3').getAttribute('src')).toBe(dataUrl));
 expect(document.body.textContent).not.toContain('base64');
 expect(document.body.textContent?.match(/\[Image #3\]/g)).toHaveLength(1);
});

it("shows a retriable failure instead of a permanently dead attachment",async()=>{
 const loadImage=vi.fn().mockRejectedValueOnce(new Error('offline')).mockResolvedValue('data:image/png;base64,aGVsbG8=');
 render(()=><FileReferenceContext.Provider value={{loadImage,resolve:async()=>[],open:()=>{},search:()=>{}}}><UserTurn text="[Image #3] [Uploaded: /tmp/a.png]"/></FileReferenceContext.Provider>);
 await waitFor(()=>expect(screen.getByRole('button',{name:'Image #3'}).title).toContain('retry'));
 fireEvent.click(screen.getByRole('button',{name:'Image #3'}));
 await waitFor(()=>expect(screen.getByAltText('Image #3')).toBeTruthy());
 expect(loadImage).toHaveBeenCalledTimes(2);
});
it("editing and resending preserves a nonconsecutive image number",()=>{
 const reuse=vi.fn().mockResolvedValue(true);
 render(()=><UserTurn text={'Inspect [Image #3]\n\n[Image #3] [Uploaded: /tmp/a.png]'} onSend={reuse}/>);
 fireEvent.click(screen.getByRole('button',{name:'Edit prompt'}));
 const editor=screen.getByRole('textbox');
 expect((editor as HTMLTextAreaElement).value).toBe('Inspect [Image #3]');
 fireEvent.input(editor,{target:{value:'Revised [Image #3]'}});
 fireEvent.click(screen.getByRole('button',{name:'Send follow-up'}));
 expect(reuse).toHaveBeenCalledWith('Revised [Image #3]\n\n[Image #3] [Uploaded: /tmp/a.png]');
});
