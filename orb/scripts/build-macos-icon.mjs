import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { execFileSync } from "node:child_process";

// Compile with Apple's tools so macOS can render the native icon material.
const root = fileURLToPath(new URL("../src-tauri/", import.meta.url));
const output = resolve(root, "gen/app-icon");
mkdirSync(output, { recursive: true });
execFileSync("xcrun", ["actool", resolve(root, "icons/Orb.icon"),
  "--compile", output, "--output-format", "human-readable-text",
  "--notices", "--warnings", "--errors", "--output-partial-info-plist", resolve(output, "partial.plist"),
  "--app-icon", "Orb", "--include-all-app-icons", "--enable-on-demand-resources", "NO",
  "--development-region", "en", "--target-device", "mac", "--minimum-deployment-target", "11.0",
  "--platform", "macosx"], { stdio: "inherit" });
