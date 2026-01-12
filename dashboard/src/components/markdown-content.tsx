"use client";

import { useState, useCallback, useEffect, useRef } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { Copy, Check, Download, Image, Loader2, FileText } from "lucide-react";
import { cn } from "@/lib/utils";
import { getRuntimeApiBase } from "@/lib/settings";
import { authHeader } from "@/lib/auth";

interface MarkdownContentProps {
  content: string;
  className?: string;
  /** Base path for resolving relative file paths (e.g., mission working directory) */
  basePath?: string;
}

// Image extensions that we can preview
const IMAGE_EXTENSIONS = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"];

// Other file extensions we recognize (for download tooltip)
const FILE_EXTENSIONS = [
  ...IMAGE_EXTENSIONS,
  ".pdf", ".txt", ".md", ".json", ".yaml", ".yml", ".xml", ".csv",
  ".log", ".sh", ".py", ".js", ".ts", ".rs", ".go", ".html", ".css",
  ".zip", ".tar", ".gz", ".mp4", ".mp3", ".wav", ".mov",
];

/** Check if a string looks like a file path */
function isFilePath(str: string): boolean {
  // Must have a file extension
  const hasExtension = FILE_EXTENSIONS.some(ext =>
    str.toLowerCase().endsWith(ext)
  );
  if (!hasExtension) return false;

  // Must look like a path (has slash or starts with common path patterns)
  const looksLikePath =
    str.includes("/") ||
    str.startsWith("./") ||
    str.startsWith("../") ||
    str.startsWith("~") ||
    /^[a-zA-Z]:/.test(str); // Windows paths

  // Or is just a filename with extension in a common directory pattern
  const isSimpleFilename = /^[\w\-_.]+\.[a-z0-9]+$/i.test(str);

  return looksLikePath || isSimpleFilename;
}

/** Check if file is an image we can preview */
function isImageFile(path: string): boolean {
  return IMAGE_EXTENSIONS.some(ext => path.toLowerCase().endsWith(ext));
}

/** Resolve a potentially relative path against a base path */
function resolvePath(path: string, basePath?: string): string {
  // Already absolute
  if (path.startsWith("/") || /^[a-zA-Z]:/.test(path)) {
    return path;
  }

  // If we have a base path, join them
  if (basePath) {
    // Remove trailing slash from base, leading ./ from path
    const cleanBase = basePath.replace(/\/+$/, "");
    const cleanPath = path.replace(/^\.\//, "");
    return `${cleanBase}/${cleanPath}`;
  }

  // Return as-is if no base path
  return path;
}

interface FilePathPreviewProps {
  path: string;
  basePath?: string;
  children: React.ReactNode;
}

/** Component that wraps file paths with hover preview/download functionality */
function FilePathPreview({ path, basePath, children }: FilePathPreviewProps) {
  const [isHovering, setIsHovering] = useState(false);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const showTimeoutRef = useRef<NodeJS.Timeout | null>(null);
  const hideTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  const resolvedPath = resolvePath(path, basePath);
  const isImage = isImageFile(path);

  // Fetch image when hovering starts
  useEffect(() => {
    if (!isHovering || !isImage || imageUrl || error) return;

    let cancelled = false;
    const fetchImage = async () => {
      setLoading(true);
      try {
        const API_BASE = getRuntimeApiBase();
        const res = await fetch(
          `${API_BASE}/api/fs/download?path=${encodeURIComponent(resolvedPath)}`,
          { headers: { ...authHeader() } }
        );

        if (!res.ok || cancelled) {
          if (!cancelled) setError(true);
          return;
        }

        const blob = await res.blob();
        if (cancelled) return;

        // Verify it's actually an image
        if (!blob.type.startsWith("image/")) {
          setError(true);
          return;
        }

        const url = URL.createObjectURL(blob);
        setImageUrl(url);
      } catch {
        if (!cancelled) setError(true);
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    fetchImage();

    return () => {
      cancelled = true;
    };
  }, [isHovering, isImage, imageUrl, error, resolvedPath]);

  // Cleanup blob URL on unmount
  useEffect(() => {
    return () => {
      if (imageUrl) URL.revokeObjectURL(imageUrl);
    };
  }, [imageUrl]);

  // Cleanup timeouts on unmount
  useEffect(() => {
    return () => {
      if (showTimeoutRef.current) clearTimeout(showTimeoutRef.current);
      if (hideTimeoutRef.current) clearTimeout(hideTimeoutRef.current);
    };
  }, []);

  const handleMouseEnter = () => {
    // Cancel any pending hide
    if (hideTimeoutRef.current) {
      clearTimeout(hideTimeoutRef.current);
      hideTimeoutRef.current = null;
    }
    // Small delay to avoid tooltips on quick mouse passes
    if (!isHovering && !showTimeoutRef.current) {
      showTimeoutRef.current = setTimeout(() => {
        setIsHovering(true);
        showTimeoutRef.current = null;
      }, 300);
    }
  };

  const handleMouseLeave = () => {
    // Cancel any pending show
    if (showTimeoutRef.current) {
      clearTimeout(showTimeoutRef.current);
      showTimeoutRef.current = null;
    }
    // Delay hiding to allow moving to tooltip
    if (!hideTimeoutRef.current) {
      hideTimeoutRef.current = setTimeout(() => {
        setIsHovering(false);
        hideTimeoutRef.current = null;
      }, 150);
    }
  };

  const handleDownload = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();

    try {
      const API_BASE = getRuntimeApiBase();
      const res = await fetch(
        `${API_BASE}/api/fs/download?path=${encodeURIComponent(resolvedPath)}`,
        { headers: { ...authHeader() } }
      );

      if (!res.ok) return;

      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = path.split("/").pop() || "download";
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch {
      // Silent fail - user can still copy the path
    }
  };

  const handleClick = (e: React.MouseEvent) => {
    // Open image in new tab on click
    if (isImage && imageUrl) {
      e.preventDefault();
      window.open(imageUrl, "_blank");
    }
  };

  return (
    <span
      className="relative inline-block"
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <code
        className={cn(
          "px-1.5 py-0.5 rounded bg-white/[0.06] text-indigo-300 text-xs font-mono",
          "cursor-pointer hover:bg-white/[0.1] transition-colors",
          isImage && imageUrl && "hover:text-indigo-200"
        )}
        onClick={handleClick}
      >
        {children}
      </code>

      {/* Hover tooltip/preview */}
      {isHovering && (
        <>
          {/* Invisible bridge to prevent gap between trigger and tooltip */}
          <div className="absolute left-0 right-0 h-2 top-full" />
          <div
            className={cn(
              "absolute z-50 mt-1 left-0",
              "bg-gray-900/95 backdrop-blur-sm border border-white/10 rounded-lg shadow-xl",
              "animate-in fade-in-0 zoom-in-95 duration-150",
              isImage ? "min-w-[200px] max-w-[400px]" : "min-w-[160px]"
            )}
            style={{
              // Prevent tooltip from going off-screen
              maxWidth: "min(400px, calc(100vw - 40px))",
            }}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
          >
          {isImage ? (
            // Image preview
            <div className="p-2">
              {loading && (
                <div className="flex items-center justify-center gap-2 py-4 text-white/50 text-xs">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  <span>Loading preview...</span>
                </div>
              )}

              {error && (
                <div className="flex items-center gap-2 p-3 text-white/40 text-xs">
                  <Image className="h-4 w-4" />
                  <span>Preview unavailable</span>
                  <button
                    onClick={handleDownload}
                    className="ml-auto flex items-center gap-1 px-2 py-1 rounded bg-white/5 hover:bg-white/10 transition-colors"
                  >
                    <Download className="h-3 w-3" />
                    <span>Download</span>
                  </button>
                </div>
              )}

              {imageUrl && !loading && (
                <>
                  {/* eslint-disable-next-line @next/next/no-img-element */}
                  <img
                    src={imageUrl}
                    alt={path.split("/").pop() || "preview"}
                    className="max-w-full max-h-[250px] object-contain rounded"
                  />
                  <div className="flex items-center justify-between mt-2 pt-2 border-t border-white/5">
                    <span className="text-[10px] text-white/30 truncate max-w-[200px]">
                      {path.split("/").pop()}
                    </span>
                    <button
                      onClick={handleDownload}
                      className="flex items-center gap-1 px-2 py-1 rounded text-[10px] text-white/50 hover:text-white/70 hover:bg-white/5 transition-colors"
                    >
                      <Download className="h-3 w-3" />
                      Download
                    </button>
                  </div>
                </>
              )}
            </div>
          ) : (
            // Non-image file: download option
            <div className="p-3 flex items-center gap-3">
              <FileText className="h-4 w-4 text-white/40 shrink-0" />
              <div className="min-w-0 flex-1">
                <div className="text-xs text-white/70 truncate">
                  {path.split("/").pop()}
                </div>
                <div className="text-[10px] text-white/30 truncate">
                  {path}
                </div>
              </div>
              <button
                onClick={handleDownload}
                className="flex items-center gap-1.5 px-2.5 py-1.5 rounded bg-white/5 hover:bg-white/10 text-xs text-white/60 hover:text-white/80 transition-colors shrink-0"
              >
                <Download className="h-3.5 w-3.5" />
                Download
              </button>
            </div>
          )}
        </div>
        </>
      )}
    </span>
  );
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

export function MarkdownContent({ content, className, basePath }: MarkdownContentProps) {
  return (
    <div className={cn("prose-glass text-sm [&_p]:my-2", className)}>
    <Markdown
      remarkPlugins={[remarkGfm]}
      components={{
        code({ className, children, ...props }) {
          const match = /language-(\w+)/.exec(className || "");
          const codeString = String(children).replace(/\n$/, "");
          const isInline = !match && !codeString.includes("\n");

          if (isInline) {
            // Check if this looks like a file path
            if (isFilePath(codeString)) {
              return (
                <FilePathPreview path={codeString} basePath={basePath}>
                  {children}
                </FilePathPreview>
              );
            }

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
