import {beforeEach, expect, it, vi} from "vitest";
vi.mock("../src/api", () => ({api: vi.fn(), getApiUrl:()=>"https://images.test", connectionVersion:()=>0}));
vi.mock("../src/localAgents", () => ({writeLocalFiles: vi.fn()}));
import {api} from "../src/api";
import {writeLocalFiles} from "../src/localAgents";
import {readImage, stageLocalImages, stageRemoteImages, imagePrompt} from "../src/imageAttachments";
beforeEach(() => vi.clearAllMocks());
it("reads binary clipboard images and writes their bytes into the local workspace", async () => {
 const image = await readImage(new File([new Uint8Array([137,80,78,71])], "pasted.png", {type:"image/png"}));
 const paths = await stageLocalImages("/workspace", [image]);
 expect(writeLocalFiles).toHaveBeenCalledWith("/workspace", [{rel:`.paloma/images/${image.name}`,content:"iVBORw==",encoding:"base64"}]);
 expect(imagePrompt("Inspect this", paths)).toContain(`[Uploaded: /workspace/.paloma/images/${image.name}]`);
});
it("rejects unsupported clipboard files rather than silently dropping them", async () => {
 await expect(readImage(new File(["svg"],"x.svg",{type:"image/svg+xml"}))).rejects.toThrow("PNG");
});
it("sends remote image uploads to the mission workspace", async () => {
 vi.mocked(api).mockResolvedValue({path:"/root/context/mission/image.png"});
 const image = await readImage(new File(["image"], "image.png", {type:"image/png"}));
 const paths = await stageRemoteImages([image], {id:"mission",workspace_id:"workspace"} as any);
 expect(paths).toEqual(["/root/context/mission/image.png"]);
 expect(api).toHaveBeenCalledWith(expect.stringContaining("workspace_id=workspace&mission_id=mission"),expect.objectContaining({method:"POST",body:expect.any(FormData)}));
});
it("routes pasted images to the chosen node instead of writing them on Core", async () => {
 vi.mocked(api).mockResolvedValue({path:"/node/uploads/image.png"});
 const image = await readImage(new File(["image"], "image.png", {type:"image/png"}));
 expect(await stageRemoteImages([image], undefined, "ashur")).toEqual(["/node/uploads/image.png"]);
 expect(api).toHaveBeenCalledWith("/api/uploads", expect.objectContaining({method:"POST",body:JSON.stringify({node_id:"ashur",name:image.name,data_base64:image.dataUrl.split(",")[1]})}));
});

it("keeps embedded images in text order and maps references to uploaded paths", async () => {
 const {readImagePaste} = await import("../src/imageAttachments");
 const result = await readImagePaste('<p>Before</p><p><img src="cid:one"></p><p>Between<img src="cid:two">After</p>', 'Before Between After', [new File(['one'], '1.png', {type:'image/png'}), new File(['two'], '2.png', {type:'image/png'})], 3);
 expect(result.text).toBe('Before\n[Image #3]\nBetween[Image #4]After');
 expect(result.images.map(image => image.reference)).toEqual([3,4]);
 expect(imagePrompt(result.text, ['/one.png','/two.png'], result.images)).toContain('[Image #4] [Uploaded: /two.png]');
});
it("keeps plain text alongside a binary clipboard image", async () => {
 const {readImagePaste} = await import("../src/imageAttachments");
 const result = await readImagePaste('', 'Look at this', [new File(['one'], '1.png', {type:'image/png'})], 1);
 expect(result.text).toBe('Look at this\n[Image #1]');
});
it("extracts data images without executing clipboard markup", async () => {
 const {readImagePaste} = await import("../src/imageAttachments");
 const fetcher = vi.spyOn(globalThis, 'fetch').mockResolvedValue({ok:true, blob:async () => new Blob(['one'], {type:'image/png'})} as Response);
 try {
  const result = await readImagePaste('<script>bad()</script><div>Hello<br><img src="data:image/png;base64,b25l">end</div>', '', [], 1);
  expect(result.text).toBe('Hello\n[Image #1]end');
  expect(result.images).toHaveLength(1);
 } finally {fetcher.mockRestore();}
});
it("reports inaccessible embedded images instead of silently losing them", async () => {
 const {readImagePaste} = await import("../src/imageAttachments");
 await expect(readImagePaste('<img src="cid:missing">', '', [], 1)).rejects.toThrow('bytes');
});
it("recognizes Proton Mail placeholders from the real clipboard format", async () => {
 const {readImagePaste} = await import("../src/imageAttachments");
 const html = '<div>Before</div><div><span class="proton-image-anchor" data-proton-embedded="embedded-33741" style="max-width: 526px;"></span></div><div>After</div>';
 await expect(readImagePaste(html, 'Before\nAfter', [], 1)).rejects.toThrow('Proton Mail copied a placeholder');
 const result = await readImagePaste(html, 'Before\nAfter', [new File(['image'], 'image.png', {type:'image/png'})], 1);
 expect(result.text).toBe('Before\n[Image #1]\nAfter');
 expect(result.images).toHaveLength(1);
});
