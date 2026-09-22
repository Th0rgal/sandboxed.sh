import {beforeEach, expect, it, vi} from "vitest";
vi.mock("../src/api", () => ({api: vi.fn()}));
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
