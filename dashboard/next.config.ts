import type { NextConfig } from "next";
import { readFileSync } from "fs";

const { version } = JSON.parse(readFileSync("./package.json", "utf-8"));

const nextConfig: NextConfig = {
  ...(process.env.STANDALONE === "true" ? { output: "standalone" as const } : {}),
  env: {
    NEXT_PUBLIC_APP_VERSION: version,
  },
  turbopack: {
    root: process.cwd(),
  },
  // Barrel-file aware tree-shaking. Without this Next has to pull the whole
  // package for a single named import in a handful of files — even though
  // the runtime doesn't use it, the initial JS bundle carries it. `lucide-
  // react` is 45 MB on disk; `react-syntax-highlighter` is 9 MB and ships
  // the full Prism language set by default. Listing them here tells Next to
  // rewrite named imports into direct deep imports at compile time.
  experimental: {
    optimizePackageImports: [
      "lucide-react",
      "react-syntax-highlighter",
      "framer-motion",
    ],
  },
};

export default nextConfig;
