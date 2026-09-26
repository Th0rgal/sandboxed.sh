import {expect,it,vi,afterEach} from "vitest";
import {rememberImagePreview,imagePreview,imagePreviewScope} from "../src/imagePreviews";
import {createFileClient} from "../src/fileResources";
import {setApiUrl,connectionVersion,bumpConnectionVersion,type Mission} from "../src/api";
afterEach(()=>{vi.restoreAllMocks();vi.unstubAllGlobals();});
const mission={id:'mission',workspace_id:'workspace',project:'test'} as Mission;
it('isolates previews by backend, connection and machine',()=>{
 const scope=imagePreviewScope('one',0,'core');
 rememberImagePreview(scope,'/tmp/a.png','data:image/png;base64,YQ==');
 expect(imagePreview(scope,'/tmp/a.png')).toContain('base64');
 for(const other of [imagePreviewScope('two',0,'core'),imagePreviewScope('one',1,'core'),imagePreviewScope('one',0,'node')]) expect(imagePreview(other,'/tmp/a.png')).toBeUndefined();
});
it('uses the uploaded preview only for the matching live connection',()=>{
 setApiUrl('https://images.test');
 rememberImagePreview(imagePreviewScope('https://images.test',connectionVersion(),'core'),'/tmp/a.png','preview');
 const client=createFileClient({mission});
 expect(client.imagePreview('/tmp/a.png')).toBe('preview');
 bumpConnectionVersion(v=>v+1);
 expect(client.imagePreview('/tmp/a.png')).toBeUndefined();
});
it('reads an initial Core upload outside the mission workspace using the authenticated upload reader',async()=>{
 setApiUrl('https://images.test');localStorage.setItem('orb.jwt','test-token');
 const fetcher=vi.spyOn(globalThis,'fetch').mockResolvedValueOnce(new Response('',{status:404})).mockResolvedValueOnce(new Response(new Uint8Array([1,2,3])));
 const create=vi.fn(()=> 'blob:preview');vi.stubGlobal("URL",class extends URL { static createObjectURL=create; });
 const client=createFileClient({mission});
 expect(await client.loadUploadedImage('/context/a.png')).toBe('blob:preview');
 expect(fetcher).toHaveBeenCalledTimes(2);
 const [first,options]=fetcher.mock.calls[0];
 expect(String(first)).toContain('workspace_id=workspace');
 expect(options?.headers).toEqual({Authorization:'Bearer test-token'});
 expect(String(fetcher.mock.calls[1][0])).toBe('https://images.test/api/fs/download?path=%2Fcontext%2Fa.png');
 expect(create.mock.calls[0]).toBeTruthy();
});
it('never sends local or remote-node paths to the Core download API',async()=>{
 const fetcher=vi.spyOn(globalThis,'fetch');
 for(const m of [{...mission,remote_node_id:'ashur'},{...mission,tags:['placement:client']}]) expect(await createFileClient({mission:m}).loadUploadedImage('/context/a.png')).toBeNull();
 expect(fetcher).not.toHaveBeenCalled();
});
it('does not retry authentication failures against another path resolver',async()=>{
 const fetcher=vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response('',{status:401}));
 await expect(createFileClient({mission}).loadUploadedImage('/context/a.png')).rejects.toThrow('retry');
 expect(fetcher).toHaveBeenCalledTimes(1);
});
it('discards an image response when the connection changes during the request',async()=>{
 let finish!:(response:Response)=>void;
 vi.spyOn(globalThis,'fetch').mockImplementation(()=>new Promise(resolve=>{finish=resolve;}));
 const pending=createFileClient({mission}).loadUploadedImage('/context/a.png');
 bumpConnectionVersion(v=>v+1);
 finish(new Response(new Uint8Array([1,2,3])));
 expect(await pending).toBeNull();
});
it('cancels a streamed image that exceeds the preview limit',async()=>{
 const cancel=vi.fn();
 const body=new ReadableStream({start(controller){controller.enqueue(new Uint8Array(21*1024*1024));},cancel});
 vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(body));
 expect(await createFileClient({mission}).loadUploadedImage('/context/a.png')).toBeNull();
 expect(cancel).toHaveBeenCalledOnce();
});
