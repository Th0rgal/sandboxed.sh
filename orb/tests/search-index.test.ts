import {expect,it} from 'vitest';
import {indexText,matchOffsets,matchRange} from '../src/searchIndex';
it('keeps block boundaries while matching across syntax tokens',()=>{
 const scope=document.createElement('div');scope.innerHTML='<p>hel<b>lo</b></p><p>world</p><button>hello</button>';document.body.append(scope);
 const index=indexText(scope);expect(matchOffsets(index.text,'helloworld',false,false)).toHaveLength(0);
 const matches=matchOffsets(index.text,'hello',false,false);expect(matches).toHaveLength(1);expect(matchRange(index,matches[0])?.toString()).toBe('hello');scope.remove();
});
it('supports literal special characters and full uncapped counts',()=>{
 expect(matchOffsets('a.b axb','a.b',false,false)).toHaveLength(1);
 expect(matchOffsets('word '.repeat(6000),'word',false,true)).toHaveLength(6000);
 expect(matchOffsets('Word wordy','word',false,true)).toHaveLength(1);
});
