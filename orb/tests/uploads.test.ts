import { describe, expect, it, vi } from "vitest";
import { prepareUploads, uploadToken, type UploadedFile } from "../src/uploads";
const file: UploadedFile = { source: { name: "photo one.png", localPath: "/Users/test/photo one.png" }, path: "/Users/test/photo one.png", destination: "local", connection: 0 };
describe("uploaded file destinations", () => {
  it("transfers a selected local file before rewriting its path for a node", async () => {
    const transfer = vi.fn(async (source, destination) => ({ source, destination, path: "/node/uploads/photo one.png", connection: 0 }));
    const result = await prepareUploads(`Inspect ${uploadToken(file.path)}`, [file], "ashur", transfer, 0);
    expect(transfer).toHaveBeenCalledWith(file.source, "ashur");
    expect(result.text).toBe('Inspect @"/node/uploads/photo one.png"');
    expect(result.text).not.toContain("/Users/");
  });
  it("does not transfer a removed reference or repeat an upload to the same destination", async () => {
    const transfer = vi.fn();
    expect((await prepareUploads("No attachment", [file], "ashur", transfer, 0)).files).toEqual([]);
    await prepareUploads(uploadToken(file.path), [file], "local", transfer, 0);
    expect(transfer).not.toHaveBeenCalled();
  });
  it("does not silently reuse a path after switching backend connections", async () => {
    const transfer = vi.fn(async () => ({ ...file, path: "/new/upload.png", connection: 1 }));
    const result = await prepareUploads(uploadToken(file.path), [file], "local", transfer, 1);
    expect(transfer).toHaveBeenCalledOnce(); expect(result.text).toContain("/new/upload.png");
  });
  it("propagates transfer failure without mutating the draft or attachment", async () => {
    const text = uploadToken(file.path);
    await expect(prepareUploads(text, [file], "ashur", async () => { throw new Error("offline"); }, 0)).rejects.toThrow("offline");
    expect(file.path).toBe("/Users/test/photo one.png");
  });
});
