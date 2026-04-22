"use client";

import { useState, useEffect, memo } from "react";
import type { CSSProperties, ReactNode } from "react";

/**
 * A code block that shows plain monospace text immediately and
 * lazy-loads `react-syntax-highlighter` (full Prism with all languages)
 * after mount.
 *
 * Why: the Prism bundle is ~80 KB gzipped and synchronously tokenizes
 * large inputs (~200–800 ms main-thread for big tool-result payloads
 * — see issue #156). Keeping it out of the initial route chunk lets
 * `/control` paint without the tokenizer attached, and defers the cost
 * until a block actually mounts. The dynamic `import()` means it only
 * lands in the user's browser when the first code block renders.
 *
 * Supersedes `LazyJsonHighlighter` for non-JSON code blocks. The
 * JSON-specific wrapper is kept for call sites that render a pile of
 * small JSON payloads and benefit from its tighter defaults.
 */

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

export interface LazyCodeBlockProps {
  children: string;
  /** Prism language tag (e.g. `"bash"`, `"typescript"`, `"json"`). */
  language: string;
  /** Optional overrides applied to both the fallback `<pre>` and the
   * final Highlighter. Keep them compatible — the fallback mirrors the
   * highlighter's final look so there's no flash on hydration. */
  customStyle?: CSSProperties;
  codeStyle?: CSSProperties;
  /** Extra UI nodes rendered alongside the code (copy button, file name
   * header, etc.). Positioned by the caller via wrapping. */
  header?: ReactNode;
  className?: string;
  /** Forwarded to Prism when present. Ignored during the plain-text
   * fallback — we don't render gutter numbers without the highlighter
   * attached, so toggling this doesn't cause layout shift on load. */
  showLineNumbers?: boolean;
}

const MONO_FONT =
  'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace';

export const LazyCodeBlock = memo(function LazyCodeBlock({
  children,
  language,
  customStyle,
  codeStyle,
  header,
  className,
  showLineNumbers,
}: LazyCodeBlockProps) {
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

  const baseStyle: CSSProperties = {
    margin: 0,
    padding: "0.75rem",
    fontSize: "0.75rem",
    borderRadius: "0.5rem",
    background: "#282c34",
    color: "#abb2bf",
    fontFamily: MONO_FONT,
    whiteSpace: "pre-wrap",
    wordBreak: "break-word",
    overflow: "hidden",
    ...customStyle,
  };

  if (!Loaded) {
    return (
      <div className={className}>
        {header}
        <pre style={baseStyle}>{children}</pre>
      </div>
    );
  }

  const { Highlighter, theme } = Loaded;
  return (
    <div className={className}>
      {header}
      <Highlighter
        language={language}
        style={theme}
        customStyle={baseStyle}
        showLineNumbers={showLineNumbers}
        codeTagProps={{
          style: {
            fontFamily: MONO_FONT,
            ...codeStyle,
          },
        }}
      >
        {children}
      </Highlighter>
    </div>
  );
});
