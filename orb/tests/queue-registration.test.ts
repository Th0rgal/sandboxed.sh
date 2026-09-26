import {afterEach,expect,it,vi} from 'vitest';
import {listQueuedMessages,setConnection,clearConnection} from '../src/api';
import {localOrigins} from '../src/localOrigins';
vi.mock('../src/localOrigins',()=>({localOrigins:vi.fn()}));
afterEach(()=>{clearConnection();vi.unstubAllGlobals();vi.resetAllMocks();});
it('does not query Core before the local mission has synchronized, then reads the server queue',async()=>{
 setConnection('http://core.test','test');
 const local={id:'new-local',local_sync_pending:true} as any;
 vi.mocked(localOrigins).mockResolvedValue([local]);
 const fetcher=vi.fn(async()=>new Response(JSON.stringify([{id:'follow-up',content:'Next',mission_id:'new-local'}])));
 vi.stubGlobal('fetch',fetcher);
 await expect(listQueuedMessages('new-local')).resolves.toEqual([]);
 expect(fetcher).not.toHaveBeenCalled();
 local.local_sync_pending=false;
 await expect(listQueuedMessages('new-local')).resolves.toMatchObject([{id:'follow-up',content:'Next'}]);
 expect(fetcher).toHaveBeenCalledTimes(1);
});
it('does not hide a missing remote mission or failures after synchronization',async()=>{
 setConnection('http://core.test','test');
 vi.mocked(localOrigins).mockResolvedValue([{id:'synced',local_sync_pending:false} as any]);
 vi.stubGlobal('fetch',vi.fn(async()=>new Response('mission not found',{status:404})));
 await expect(listQueuedMessages('remote')).rejects.toThrow('404');
 await expect(listQueuedMessages('synced')).rejects.toThrow('404');
});
