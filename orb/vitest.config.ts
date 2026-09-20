import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";
export default defineConfig({ plugins: [solid()], test: { environment: "jsdom", include: ["tests/**/*.test.ts", "tests/**/*.test.tsx"], setupFiles: ["./tests/setup.ts"] } });
