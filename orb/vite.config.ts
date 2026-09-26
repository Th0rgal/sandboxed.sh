import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";

function palomaPub(): Plugin {
  return {
    name: "paloma-pub",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (req.url !== "/__paloma_pub") return next();
        try {
          res.setHeader("Content-Type", "text/plain");
          res.end(fs.readFileSync(path.join(os.homedir(), ".ssh/paloma.pub"), "utf8"));
        } catch {
          res.statusCode = 404;
          res.end("");
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [solid(), palomaPub()],
  clearScreen: false,
  server: { port: 1430, strictPort: true },
  build: { target: "safari15" },
});
