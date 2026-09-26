import { test, expect } from "@playwright/test";
test("file picker transfers binary bytes, then relocates the reference when the machine changes", async ({page}) => {
 const requests: Array<{node_id:string;name:string;data_base64:string}> = [];
 await page.route("**/api/**", route => {
  if (!route.request().url().endsWith("/uploads")) return route.fulfill({json:{}});
  const body = route.request().postDataJSON(); requests.push(body);
  return route.fulfill({json:{name:body.name,path:`/${body.node_id}/uploads/${body.name}`,size:4,sha256:"test"}});
 });
 await page.goto("/tests/uploads.html");
 await page.getByTitle("Add context").click();
 await expect(page.getByText("Upload file or image…")).toBeVisible();
 const chooserPromise=page.waitForEvent("filechooser"); await page.getByText("Upload file or image…").click();
 await (await chooserPromise).setFiles({name:"sample one.bin",mimeType:"application/octet-stream",buffer:Buffer.from([0,255,1,2])});
 await expect(page.getByPlaceholder("Describe a task")).toHaveValue('@"/core/uploads/sample one.bin" ');
 expect(requests[0].data_base64).toBe("AP8BAg==");
 await page.screenshot({path:"/tmp/orb-upload-composer.png"});
 await page.getByLabel("Machine").selectOption("ashur");
 await page.getByTitle("Send",{exact:true}).click();
 await expect(page.getByLabel("Sent prompt")).toHaveText('@"/ashur/uploads/sample one.bin"');
 expect(requests.map(r=>r.node_id)).toEqual(["core","ashur"]);
});
