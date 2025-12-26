"use client";

import { useState, useCallback } from "react";
import Markdown from "react-markdown";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { Copy, Check } from "lucide-react";
import { cn } from "@/lib/utils";

interface MarkdownContentProps {
  content: string;
  className?: string;
}

function CopyCodeButton({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for older browsers
      const textarea = document.createElement("textarea");
      textarea.value = code;
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand("copy");
      document.body.removeChild(textarea);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [code]);

  return (
    <button
      onClick={handleCopy}
      className={cn(
        "absolute right-2 top-2 p-1.5 rounded-md transition-all",
        "bg-white/[0.05] hover:bg-white/[0.1]",
        "text-white/40 hover:text-white/70",
        "opacity-0 group-hover:opacity-100"
      )}
      title={copied ? "Copied!" : "Copy code"}
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-emerald-400" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </button>
  );
}

export function MarkdownContent({ content, className }: MarkdownContentProps) {
  return (
    <div className={cn("prose-glass text-sm [&_p]:my-2", className)}>
    <Markdown
      components={{
        code({ className, children, ...props }) {
          const match = /language-(\w+)/.exec(className || "");
          const codeString = String(children).replace(/\n$/, "");
          const isInline = !match && !codeString.includes("\n");

          if (isInline) {
            return (
              <code
                className="px-1.5 py-0.5 rounded bg-white/[0.06] text-indigo-300 text-xs font-mono"
                {...props}
              >
                {children}
              </code>
            );
          }

          return (
            <div className="relative group my-3 rounded-lg overflow-hidden">
              <CopyCodeButton code={codeString} />
              {match ? (
                <SyntaxHighlighter
                  style={oneDark}
                  language={match[1]}
                  PreTag="div"
                  customStyle={{
                    margin: 0,
                    padding: "1rem",
                    fontSize: "0.75rem",
                    borderRadius: "0.5rem",
                    background: "rgba(0, 0, 0, 0.3)",
                  }}
                  codeTagProps={{
                    style: {
                      fontFamily:
                        'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
                    },
                  }}
                >
                  {codeString}
                </SyntaxHighlighter>
              ) : (
                <pre className="p-4 bg-black/30 rounded-lg overflow-x-auto">
                  <code className="text-xs font-mono text-white/80">{codeString}</code>
                </pre>
              )}
              {match && (
                <div className="absolute left-3 top-2 text-[10px] text-white/30 uppercase tracking-wider">
                  {match[1]}
                </div>
              )}
            </div>
          );
        },
        pre({ children }) {
          // The code component handles everything, so just pass through
          return <>{children}</>;
        },
      }}
    >
      {content}
    </Markdown>
    </div>
  );
}
