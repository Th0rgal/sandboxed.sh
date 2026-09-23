import {afterEach,expect,it,vi} from "vitest";
import {cleanup,render,screen,fireEvent,waitFor} from "@solidjs/testing-library";
import {MdView} from "../src/Markdown";
import {highlightCode} from "../src/codeHighlight";
afterEach(()=>{cleanup();vi.unstubAllGlobals();});
it("copies just the code, preserving indentation and blank lines",async()=>{
 const writeText=vi.fn().mockResolvedValue(undefined);
 vi.stubGlobal("navigator",{clipboard:{writeText}});
 render(()=><MdView text={'```python\ndef hello():\n    return "hello"\n\nhello()\n```'}/>);
 fireEvent.click(screen.getByRole('button',{name:'Copy code'}));
 await waitFor(()=>expect(screen.getByRole('status').textContent).toBe('Copied'));
 expect(writeText).toHaveBeenCalledWith('def hello():\n    return "hello"\n\nhello()');
});
it("reports refused clipboard writes locally",async()=>{
 vi.stubGlobal("navigator",{clipboard:{writeText:vi.fn().mockRejectedValue(new Error('denied'))}});
 render(()=><MdView text={'```\ncode\n```'}/>);
 fireEvent.click(screen.getByRole('button',{name:'Copy code'}));
 await waitFor(()=>expect(screen.getByRole('status').textContent).toContain('denied'));
});
it("highlights declared languages and escapes executable markup",()=>{
 expect(highlightCode('def hello():\n    return "hello"','python')).toContain('token keyword');
 expect(highlightCode('const x = 1','js')).toContain('token keyword');
 expect(highlightCode('<img src=x onerror="alert(1)">','html')).not.toContain('<img');
 expect(highlightCode('plain','unknown')).toBeNull();
 expect(highlightCode('x'.repeat(100001),'python')).toBeNull();
});
