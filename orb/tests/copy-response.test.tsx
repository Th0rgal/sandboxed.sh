import {render,fireEvent,waitFor} from '@solidjs/testing-library';
import {it,expect,vi} from 'vitest';
import {Transcript} from '../src/Transcript';
it('copies the complete response as Markdown and confirms success',async()=>{
 const writeText=vi.fn().mockResolvedValue(undefined);
 Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText}});
 const text='**Answer**\n\n> First\n>\n> Second';
 const {getByRole}=render(()=><Transcript items={[{kind:'text',key:'a',text}]}/>);
 fireEvent.click(getByRole('button',{name:'Copy response'}));
 await waitFor(()=>expect(getByRole('button',{name:'Response copied'})).toBeTruthy());
 expect(writeText).toHaveBeenCalledExactlyOnceWith(text);
});
it('shows clipboard failures without claiming success',async()=>{
 Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:vi.fn().mockRejectedValue(new Error('Denied'))}});
 const {getByRole,queryByRole,getByText}=render(()=><Transcript items={[{kind:'text',key:'a',text:'Answer'}]}/>);
 fireEvent.click(getByRole('button',{name:'Copy response'}));
 await waitFor(()=>expect(getByText(/Copy was refused/)).toBeTruthy());
 expect(queryByRole('button',{name:'Response copied'})).toBeNull();
});
