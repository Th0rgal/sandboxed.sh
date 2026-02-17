"use client";

import { useState, useEffect, memo } from "react";
import type { CSSProperties } from "react";

/**
 * A JSON code block that shows plain monospace text immediately and
 * lazy-loads SyntaxHighlighter after mount. This avoids the ~200-800ms
 * synchronous Prism tokenization that blocks the main thread when
 * expanding tool calls. See issue #156.
 */

// Lazy-loaded module — only fetched on first render of a highlighted block.
let highlighterModulePromise: Promise<{
  Highlighter: typeof import("react-syntax-highlighter").default;
  theme: Record<string, CSSProperties>;
}> | null = null;

function getHighlighterModule() {
  if (!highlighterModulePromise) {
    highlighterModulePromise = Promise.all([
      import("react-syntax-highlighter").then((m) => m.Prism),
      import("react-syntax-highlighter/dist/esm/styles/prism").then(
        (m) => m.oneDark
      ),
    ]).then(([Highlighter, theme]) => ({ Highlighter, theme }));
  }
  return highlighterModulePromise;
}

interface LazyJsonHighlighterProps {
  children: string;
  background?: string;
  textColor?: string;
}

export const LazyJsonHighlighter = memo(function LazyJsonHighlighter({
  children,
  background = "rgba(0, 0, 0, 0.2)",
  textColor,
}: LazyJsonHighlighterProps) {
  const [Loaded, setLoaded] = useState<{
    Highlighter: typeof import("react-syntax-highlighter").default;
    theme: Record<string, CSSProperties>;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    getHighlighterModule().then((mod) => {
      if (!cancelled) setLoaded(mod);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const monoFont =
    'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace';

  // Before the highlighter loads, show plain monospace text. This renders
  // in <1ms vs 200-800ms for SyntaxHighlighter, making expand feel instant.
  if (!Loaded) {
    return (
      <pre
        style={{
          margin: 0,
          padding: "0.5rem",
          fontSize: "0.75rem",
          borderRadius: "0.25rem",
          background,
          color: textColor ?? "#abb2bf",
          fontFamily: monoFont,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          overflow: "hidden",
        }}
      >
        {children}
      </pre>
    );
  }

  const { Highlighter, theme } = Loaded;
  return (
    <Highlighter
      language="json"
      style={theme}
      customStyle={{
        margin: 0,
        padding: "0.5rem",
        fontSize: "0.75rem",
        borderRadius: "0.25rem",
        background,
      }}
      codeTagProps={{
        style: {
          fontFamily: monoFont,
          color: textColor,
        },
      }}
    >
      {children}
    </Highlighter>
  );
});
