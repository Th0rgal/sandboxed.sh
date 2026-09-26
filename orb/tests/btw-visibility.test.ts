import {afterEach,expect,it,vi} from 'vitest';
import {listMissions,listProjectMissions,getMission,isBtwMission,setConnection,clearConnection} from '../src/api';
afterEach(()=>{clearConnection();vi.unstubAllGlobals();});
it('hides side agents in both navigation lists without hiding their transcript endpoint',async()=>{
 setConnection('https://btw.test','test');
 const parent={id:'parent',title:'Main',project:'verity',tags:[]};
 const side={id:'side',title:'Generated title',project:'verity',tags:['fork-workspace:parent','btw-parent:parent']};
 const fork={id:'fork',project:'verity',tags:['fork-workspace:parent']};
 vi.stubGlobal('fetch',vi.fn(async(url:string)=>Response.json(url.endsWith('/side')?side:[parent,side,fork])));
 expect((await listMissions()).map(m=>m.id)).toEqual(['parent','fork']);
 expect((await listProjectMissions('verity')).map(m=>m.id)).toEqual(['parent','fork']);
 expect(await getMission('side')).toEqual(side);
 expect(isBtwMission({tags:['btw-parent:parent']})).toBe(true);
 expect(isBtwMission({})).toBe(false);
});
