import {afterEach,expect,it,vi} from 'vitest';
import {getMission,listMissions,setConnection,clearConnection} from '../src/api';
afterEach(()=>{delete (window as any).__TAURI_INTERNALS__;clearConnection();vi.unstubAllGlobals();});
it('shows a locally journaled mission and its text while Core is offline',async()=>{
 setConnection('http://offline.test','token');
 const mission={id:'local-id',title:'Offline task',status:'active',created_at:'now',updated_at:'now',history:[{role:'user',content:'Do work'},{role:'assistant',content:'Working'}],local_sync_pending:true};
 const invoke=vi.fn(async()=>[mission]);(window as any).__TAURI_INTERNALS__={invoke};
 const fetcher=vi.fn(async()=>{throw new TypeError('offline');});vi.stubGlobal('fetch',fetcher);
 expect(await getMission(mission.id)).toEqual(mission);expect(fetcher).not.toHaveBeenCalled();
 expect(await listMissions()).toEqual([mission]);
 expect(invoke).toHaveBeenCalledWith('local_origin_list',expect.objectContaining({connection:{api_url:'http://offline.test',token:'token'}}));
});
it('does not replace a newer Core conversation with a completed local snapshot',async()=>{
 setConnection('http://online.test','token');
 const old={id:'id',status:'awaiting_user',history:[],local_sync_pending:false};(window as any).__TAURI_INTERNALS__={invoke:vi.fn(async()=>[old])};
 vi.stubGlobal('fetch',vi.fn(async()=>new Response(JSON.stringify({...old,status:'completed',title:'Updated on Core'}))));
 expect((await getMission('id')).title).toBe('Updated on Core');
});
