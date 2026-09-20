import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests", testMatch: "*.browser.spec.ts", workers: 1,
  use: { baseURL: "http://127.0.0.1:1431", viewport: { width: 1100, height: 900 } },
  webServer: { command: "pnpm dev --host 127.0.0.1 --port 1431", url: "http://127.0.0.1:1431", reuseExistingServer: false },
});
