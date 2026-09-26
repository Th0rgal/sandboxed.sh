import { expect, it, vi, beforeEach } from 'vitest';
vi.mock('../src/api',()=>({ api:vi.fn(),getApiUrl:()=> 'https://core',getMission:vi.fn(),connectionVersion:()=>0 }));
import { api, getMission } from '../src/api';
import { moveMission, readCutMission } from '../src/missionMove';
const id='524754fe-3acc-42c1-ba7f-89ee2d296bcb';
beforeEach(()=>vi.clearAllMocks());
it('accepts only a mission cut from the same backend',()=>{
 expect(readCutMission('orb:cut-mission:'+JSON.stringify({id,backend:'https://core'}),'https://core')).toBe(id);
 expect(readCutMission('orb:cut-mission:'+JSON.stringify({id,backend:'https://other'}),'https://core')).toBeNull();
 expect(readCutMission('ordinary text','https://core')).toBeNull();
 expect(readCutMission('orb:cut-mission:{','https://core')).toBeNull();
});
it('preserves placement and other tags when moving to a folder',async()=>{
 vi.mocked(getMission).mockResolvedValue({id,tags:['placement:client','orb-folder:old','writer']} as any);
 await moveMission(id,'other-project','notes/review');
 const request=vi.mocked(api).mock.calls[0][1]!;
 expect(JSON.parse(request.body as string)).toEqual({project:'other-project',tags:['placement:client','writer','orb-folder:notes/review']});
});
it('removes folder classification when pasting at project root and propagates rejection',async()=>{
 vi.mocked(getMission).mockResolvedValue({id,tags:['orb-folder:old','keep']} as any);
 vi.mocked(api).mockRejectedValueOnce(new Error('busy'));
 await expect(moveMission(id,'project','')).rejects.toThrow('busy');
 expect(JSON.parse(vi.mocked(api).mock.calls[0][1]!.body as string)).toEqual({project:'project',tags:['keep']});
});
