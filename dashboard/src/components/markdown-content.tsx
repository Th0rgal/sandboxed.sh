"use client";

import { useState, useCallback, useEffect, useMemo, memo } from "react";
import { createRoot } from "react-dom/client";
import Markdown, { Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { Copy, Check, Download, Image, X, FileText, File, FileCode, FileArchive } from "lucide-react";
import { cn } from "@/lib/utils";
import { getRuntimeApiBase } from "@/lib/settings";
import { authHeader } from "@/lib/auth";

interface MarkdownContentProps {
  content: string;
  className?: string;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}

const IMAGE_EXTENSIONS = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"];
const FILE_EXTENSIONS = [
  ...IMAGE_EXTENSIONS,
  ".pdf", ".txt", ".md", ".json", ".yaml", ".yml", ".xml", ".csv",
  ".log", ".sh", ".py", ".js", ".ts", ".rs", ".go", ".html", ".css",
  ".zip", ".tar", ".gz", ".mp4", ".mp3", ".wav", ".mov",
];
const CODE_EXTENSIONS = [".sh", ".py", ".js", ".ts", ".rs", ".go", ".html", ".css", ".json", ".yaml", ".yml", ".xml"];
const ARCHIVE_EXTENSIONS = [".zip", ".tar", ".gz"];

// Global cache for fetched image URLs with automatic cleanup
// Uses a simple LRU-style eviction: when cache exceeds limit, oldest entries are revoked
const IMAGE_CACHE_LIMIT = 50;
const imageUrlCache = new Map<string, string>();

function cacheImageUrl(path: string, url: string): void {
  // If already cached, revoke the duplicate URL and update access order
  if (imageUrlCache.has(path)) {
    // Revoke the incoming duplicate URL to prevent memory leak from concurrent fetches
    URL.revokeObjectURL(url);
    const existingUrl = imageUrlCache.get(path)!;
    imageUrlCache.delete(path);
    imageUrlCache.set(path, existingUrl);
    return;
  }

  // Evict oldest entries if at limit
  while (imageUrlCache.size >= IMAGE_CACHE_LIMIT) {
    const oldestKey = imageUrlCache.keys().next().value;
    if (oldestKey) {
      const oldUrl = imageUrlCache.get(oldestKey);
      if (oldUrl) {
        URL.revokeObjectURL(oldUrl);
      }
      imageUrlCache.delete(oldestKey);
    }
  }

  imageUrlCache.set(path, url);
}

function isFilePath(str: string): boolean {
  const hasExtension = FILE_EXTENSIONS.some(ext => str.toLowerCase().endsWith(ext));
  if (!hasExtension) return false;
  const looksLikePath = str.includes("/") || str.startsWith("./") || str.startsWith("../") || str.startsWith("~") || /^[a-zA-Z]:/.test(str);
  const isSimpleFilename = /^[\w\-_.]+\.[a-z0-9]+$/i.test(str);
  return looksLikePath || isSimpleFilename;
}

function isImageFile(path: string): boolean {
  return IMAGE_EXTENSIONS.some(ext => path.toLowerCase().endsWith(ext));
}

function isCodeFile(path: string): boolean {
  return CODE_EXTENSIONS.some(ext => path.toLowerCase().endsWith(ext));
}

function isArchiveFile(path: string): boolean {
  return ARCHIVE_EXTENSIONS.some(ext => path.toLowerCase().endsWith(ext));
}

function getFileIcon(path: string) {
  if (isImageFile(path)) return Image;
  if (isCodeFile(path)) return FileCode;
  if (isArchiveFile(path)) return FileArchive;
  if (path.toLowerCase().endsWith(".txt") || path.toLowerCase().endsWith(".md") || path.toLowerCase().endsWith(".log")) return FileText;
  return File;
}

function resolvePath(path: string, basePath?: string): string {
  if (path.startsWith("/") || /^[a-zA-Z]:/.test(path)) {
    if (basePath) {
      const cleanBase = basePath.replace(/\/+$/, "");
      const match = cleanBase.match(/\/workspaces\/mission-[^/]+$/);
      if (match && path.startsWith(match[0])) {
        return `${cleanBase}${path.slice(match[0].length)}`;
      }
    }
    return path;
  }
  if (basePath) {
    const cleanBase = basePath.replace(/\/+$/, "");
    const cleanPath = path.replace(/^\.\//, "");
    return `${cleanBase}/${cleanPath}`;
  }
  return path;
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

// Imperative modal - rendered outside React's component tree
function showFilePreviewModal(
  path: string,
  resolvedPath: string,
  workspaceId?: string,
  missionId?: string
) {
  // Prevent multiple modals
  if (document.getElementById("file-preview-modal-root")) return;

  const container = document.createElement("div");
  container.id = "file-preview-modal-root";
  document.body.appendChild(container);

  const root = createRoot(container);

  const cleanup = () => {
    root.unmount();
    container.remove();
  };

  root.render(
    <FilePreviewModalContent
      path={path}
      resolvedPath={resolvedPath}
      workspaceId={workspaceId}
      missionId={missionId}
      onClose={cleanup}
    />
  );
}

interface FilePreviewModalContentProps {
  path: string;
  resolvedPath: string;
  workspaceId?: string;
  missionId?: string;
  onClose: () => void;
}

function FilePreviewModalContent({
  path,
  resolvedPath,
  workspaceId,
  missionId,
  onClose,
}: FilePreviewModalContentProps) {
  const isImage = isImageFile(path);
  const FileIcon = getFileIcon(path);
  const fileName = path.split("/").pop() || "file";

  const [imageUrl, setImageUrl] = useState<string | null>(imageUrlCache.get(resolvedPath) || null);
  const [loading, setLoading] = useState(!imageUrl && isImage);
  const [error, setError] = useState<string | null>(null);
  const [fileSize, setFileSize] = useState<number | null>(null);
  const [downloading, setDownloading] = useState(false);

  // Fetch image on mount
  useEffect(() => {
    if (!isImage || imageUrl) return;

    let cancelled = false;
    const fetchImage = async () => {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      const downloadUrl = `${API_BASE}/api/fs/download?${params.toString()}`;

      try {
        const res = await fetch(downloadUrl, { headers: { ...authHeader() } });
        if (!res.ok) {
          if (!cancelled) setError(`Failed to load (${res.status})`);
          if (!cancelled) setLoading(false);
          return;
        }
        const blob = await res.blob();
        if (!cancelled) setFileSize(blob.size);
        const url = URL.createObjectURL(blob);
        cacheImageUrl(resolvedPath, url);
        if (!cancelled) setImageUrl(url);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : "Failed to load");
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    fetchImage();
    return () => { cancelled = true; };
  }, [isImage, imageUrl, resolvedPath]);

  // Escape key handler
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleDownload = async () => {
    setDownloading(true);
    try {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      const res = await fetch(`${API_BASE}/api/fs/download?${params.toString()}`, {
        headers: { ...authHeader() },
      });
      if (!res.ok) {
        setError(`Download failed (${res.status})`);
        return;
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = fileName;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Download failed");
    } finally {
      setDownloading(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm pointer-events-none" />
      <div
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "relative rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl",
          "animate-in fade-in zoom-in-95 duration-200",
          isImage ? "max-w-3xl w-full" : "max-w-md w-full"
        )}
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-white/[0.06]">
          <div className="flex items-center gap-3 min-w-0">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-indigo-500/10">
              <FileIcon className="h-4 w-4 text-indigo-400" />
            </div>
            <div className="min-w-0">
              <h3 className="text-sm font-semibold text-white truncate">{fileName}</h3>
              <p className="text-xs text-white/40 truncate">{path}</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors shrink-0 ml-3"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="p-5">
          {isImage ? (
            <div className="space-y-4">
              <div className="relative min-h-[200px] rounded-xl overflow-hidden bg-black/20 flex items-center justify-center">
                {loading && (
                  <div className="absolute inset-0 flex flex-col items-center justify-center gap-3">
                    <div className="w-full max-w-[300px] h-[200px] rounded-lg bg-white/[0.03] animate-pulse" />
                    <span className="text-xs text-white/40">Loading preview...</span>
                  </div>
                )}
                {error && !loading && (
                  <div className="flex flex-col items-center justify-center gap-3 py-8">
                    <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-red-500/10">
                      <Image className="h-6 w-6 text-red-400" />
                    </div>
                    <span className="text-sm text-white/50">{error}</span>
                  </div>
                )}
                {imageUrl && !loading && (
                  /* eslint-disable-next-line @next/next/no-img-element */
                  <img src={imageUrl} alt={fileName} className="max-w-full max-h-[60vh] object-contain" />
                )}
              </div>
              <div className="flex items-center justify-between pt-2 border-t border-white/[0.06]">
                <div className="text-xs text-white/40">{fileSize ? formatFileSize(fileSize) : "Image file"}</div>
                <button
                  onClick={handleDownload}
                  disabled={downloading}
                  className={cn(
                    "flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium transition-colors",
                    "bg-indigo-500 hover:bg-indigo-600 text-white",
                    downloading && "opacity-50 cursor-not-allowed"
                  )}
                >
                  <Download className={cn("h-4 w-4", downloading && "animate-pulse")} />
                  {downloading ? "Downloading..." : "Download"}
                </button>
              </div>
            </div>
          ) : (
            <div className="space-y-4">
              <div className="flex flex-col items-center justify-center py-6 gap-4">
                <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.04]">
                  <FileIcon className="h-8 w-8 text-white/40" />
                </div>
                <div className="text-center">
                  <div className="text-sm text-white/70">{fileName}</div>
                  <div className="text-xs text-white/40 mt-1">{path.split(".").pop()?.toUpperCase()} file</div>
                </div>
              </div>
              <button
                onClick={handleDownload}
                disabled={downloading}
                className={cn(
                  "w-full flex items-center justify-center gap-2 px-4 py-3 rounded-xl text-sm font-medium transition-colors",
                  "bg-indigo-500 hover:bg-indigo-600 text-white",
                  downloading && "opacity-50 cursor-not-allowed"
                )}
              >
                <Download className={cn("h-4 w-4", downloading && "animate-pulse")} />
                {downloading ? "Downloading..." : "Download File"}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
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
      {copied ? <Check className="h-3.5 w-3.5 text-emerald-400" /> : <Copy className="h-3.5 w-3.5" />}
    </button>
  );
}

// Memoized to prevent re-renders when parent re-renders with same props
export const MarkdownContent = memo(function MarkdownContent({
  content,
  className,
  basePath,
  workspaceId,
  missionId,
}: MarkdownContentProps) {
  // Memoize components object to prevent react-markdown from re-creating DOM on every render
  const components: Components = useMemo(() => ({
    a({ href, children, ...props }) {
      return (
        <a
          href={href}
          target="_blank"
          rel="noopener noreferrer"
          className="text-indigo-400 hover:text-indigo-300 underline underline-offset-2 transition-colors"
          {...props}
        >
          {children}
        </a>
      );
    },
    code({ className: codeClassName, children, ...props }) {
      const match = /language-(\w+)/.exec(codeClassName || "");
      const codeString = String(children).replace(/\n$/, "");
      const isInline = !match && !codeString.includes("\n");

      if (isInline) {
        if (isFilePath(codeString)) {
          return (
            <code
              className={cn(
                "px-1.5 py-0.5 rounded bg-white/[0.06] text-indigo-300 text-xs font-mono",
                "cursor-pointer hover:bg-white/[0.1] hover:text-indigo-200 transition-colors"
              )}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                showFilePreviewModal(
                  codeString,
                  resolvePath(codeString, basePath),
                  workspaceId,
                  missionId
                );
              }}
              title="Click to preview"
            >
              {children}
            </code>
          );
        }
        return (
          <code className="px-1.5 py-0.5 rounded bg-white/[0.06] text-indigo-300 text-xs font-mono" {...props}>
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
              customStyle={{ margin: 0, padding: "1rem", fontSize: "0.75rem", borderRadius: "0.5rem", background: "rgba(0, 0, 0, 0.3)" }}
              codeTagProps={{ style: { fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace' } }}
            >
              {codeString}
            </SyntaxHighlighter>
          ) : (
            <pre className="p-4 bg-black/30 rounded-lg overflow-x-auto">
              <code className="text-xs font-mono text-white/80">{codeString}</code>
            </pre>
          )}
          {match && (
            <div className="absolute left-3 top-2 text-[10px] text-white/30 uppercase tracking-wider">{match[1]}</div>
          )}
        </div>
      );
    },
    pre({ children }) {
      return <>{children}</>;
    },
  }), [basePath, workspaceId, missionId]);

  // Memoize remarkPlugins array to prevent recreation
  const plugins = useMemo(() => [remarkGfm], []);

  return (
    <div className={cn("prose-glass text-sm [&_p]:my-2", className)}>
      <Markdown remarkPlugins={plugins} components={components}>
        {content}
      </Markdown>
    </div>
  );
});
