"use client";

import { useState, useCallback, useEffect, useLayoutEffect, useMemo, useRef, memo } from "react";
import { createRoot } from "react-dom/client";
import Markdown, { Components, defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { LazyCodeBlock } from "./lazy-code-block";
import { Copy, Check, Download, Image as ImageIcon, X, FileText, File, FileCode, FileArchive } from "lucide-react";
import { cn } from "@/lib/utils";
import { getRuntimeApiBase } from "@/lib/settings";
import { authHeader } from "@/lib/auth";
import { transformRichTags } from "@/lib/rich-tags";
import { remarkMissionLinks } from "@/lib/remark-mission-links";
import { MissionChip } from "@/components/hermes/mission-chip";
import {
  FILE_EXTENSIONS,
  isMarkdownFile,
  isTextPreviewableFile,
  isImageFile,
  isCodeFile,
  isArchiveFile,
} from "@/lib/file-extensions";

interface MarkdownContentProps {
  content: string;
  className?: string;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
  /** Opt-in (Hermes chat): render bare mission UUIDs as clickable chips. */
  missionLinks?: boolean;
}

// Global, refcounted cache of fetched image blob URLs.
//
// Why refcounted: the previous implementation revoked the oldest URL on
// LRU eviction, but that URL was often still the `src=` of a mounted
// `<img>` element somewhere on the page (the URL lives in component
// state, not just in this map). Revoking it under the running DOM made
// the browser show its broken-image icon — but a click-through to the
// preview modal triggered a fresh fetch and worked, which matched the
// reported "image fails until I click the broken icon" symptom exactly.
//
// New contract: every consumer that uses a URL from this cache must call
// `acquireCachedImageUrl` (or `cacheAndAcquireImageUrl` if it just
// fetched) and pair it with a later `releaseImageUrl`. The cache only
// revokes a URL when its refcount reaches zero AND the entry has been
// LRU-evicted, so a URL stuck in a mounted `<img>` keeps working until
// that `<img>` unmounts.
const IMAGE_CACHE_LIMIT = 50;

interface ImageCacheEntry {
  url: string;
  refCount: number;
  /** Marked when LRU-evicted but kept alive because refCount > 0. The
   *  next `releaseImageUrl` that drops refCount to 0 will revoke and
   *  drop the entry instead of leaving it indefinitely. */
  evicted: boolean;
}

const imageUrlCache = new Map<string, ImageCacheEntry>();

/** Try to read a cached URL for `path` and acquire a reference. Returns
 *  null when not cached. Callers must pair every successful return with
 *  exactly one `releaseImageUrl(path)` on unmount. */
function acquireCachedImageUrl(path: string): string | null {
  const entry = imageUrlCache.get(path);
  if (!entry) return null;
  entry.refCount++;
  // Re-acquired by a new consumer, so the entry is back to useful —
  // clear any stale eviction mark from a prior overflow that didn't
  // actually need to revoke this URL after all.
  entry.evicted = false;
  return entry.url;
}

/** Insert a freshly-fetched blob URL and acquire one reference. Caller
 *  owns the reference and must release it on unmount. Concurrent fetches
 *  for the same path collapse onto the existing entry — the duplicate
 *  URL is revoked here so the caller's later `releaseImageUrl` decrements
 *  the canonical entry's refCount instead of leaking. */
function cacheAndAcquireImageUrl(path: string, url: string): string {
  const existing = imageUrlCache.get(path);
  if (existing) {
    URL.revokeObjectURL(url);
    existing.refCount++;
    // The entry is being acquired again, so it's clearly still useful —
    // clear any prior eviction mark so a subsequent release doesn't
    // revoke a URL we're actively re-using.
    existing.evicted = false;
    return existing.url;
  }

  // LRU eviction strategy:
  //   1. Drop refCount=0 entries first — they're safe to revoke immediately.
  //   2. If everything is referenced, mark only the OLDEST entry (first
  //      in insertion order) as `evicted`, so its last consumer's
  //      `releaseImageUrl` revokes it. We don't mark more than one —
  //      that would needlessly destroy cache effectiveness for entries
  //      that callers might still re-acquire. If the working set really
  //      exceeds the cap, future inserts will mark additional entries
  //      one-by-one as they overflow.
  //   3. The cache may temporarily exceed `IMAGE_CACHE_LIMIT` when all
  //      entries are referenced; that's fine and self-corrects as
  //      consumers unmount.
  while (imageUrlCache.size >= IMAGE_CACHE_LIMIT) {
    let dropped = false;
    for (const [key, entry] of imageUrlCache) {
      if (entry.refCount === 0) {
        URL.revokeObjectURL(entry.url);
        imageUrlCache.delete(key);
        dropped = true;
        break;
      }
    }
    if (dropped) continue;

    // No droppable entries — every URL is live. Mark only the oldest
    // for revoke-on-last-release and stop.
    const oldestKey = imageUrlCache.keys().next().value;
    if (oldestKey) {
      const oldest = imageUrlCache.get(oldestKey);
      if (oldest && !oldest.evicted) {
        oldest.evicted = true;
      }
    }
    break;
  }

  imageUrlCache.set(path, { url, refCount: 1, evicted: false });
  return url;
}

/** Decrement the refcount for `path`. If it reaches zero AND the entry
 *  was previously LRU-evicted, revoke the URL and drop it. */
function releaseImageUrl(path: string): void {
  const entry = imageUrlCache.get(path);
  if (!entry) return;
  entry.refCount = Math.max(0, entry.refCount - 1);
  if (entry.refCount === 0 && entry.evicted) {
    URL.revokeObjectURL(entry.url);
    imageUrlCache.delete(path);
  }
}

// Rendered dimensions of inline images, keyed by resolved path. Inline
// previews live inside a virtualized timeline: rows unmount as the user
// scrolls away and remount on the way back, and every remount that paints
// a placeholder whose height differs from the final image shifts all rows
// below it (the virtualizer re-measures both states). Recording the
// laid-out size on first load lets remounts (and the loading skeleton)
// reserve the exact final box, so re-measures are no-ops and the scroll
// position stays put.
const imageDimsCache = new Map<string, { width: number; height: number }>();

function isFilePath(str: string): boolean {
  const hasExtension = FILE_EXTENSIONS.some(ext => str.toLowerCase().endsWith(ext));
  if (!hasExtension) return false;
  const looksLikePath = str.includes("/") || str.startsWith("./") || str.startsWith("../") || str.startsWith("~") || /^[a-zA-Z]:/.test(str);
  const isSimpleFilename = /^[\w\-_.]+\.[a-z0-9]+$/i.test(str);
  return looksLikePath || isSimpleFilename;
}

function isRichFileLinkHref(href: string): boolean {
  if (!href || href.startsWith("#")) return false;
  if (href.startsWith("sandboxed-file://")) return true;
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) {
    return /^[a-zA-Z]:/.test(href);
  }
  return isFilePath(href.split(/[?#]/, 1)[0]);
}

function getFileIcon(path: string) {
  if (isImageFile(path)) return ImageIcon;
  if (isCodeFile(path)) return FileCode;
  if (isArchiveFile(path)) return FileArchive;
  if (path.toLowerCase().endsWith(".txt") || path.toLowerCase().endsWith(".md") || path.toLowerCase().endsWith(".log")) return FileText;
  return File;
}

// Sentinel class applied by `rehypeMarkStandaloneLinks` to links that are the
// sole content of their paragraph/list-item. Only such "standalone" file links
// render as the full download card; file links mentioned mid-prose render as a
// compact inline chip so they don't break the surrounding text flow.
const STANDALONE_LINK_CLASS = "__standalone-link";

/* eslint-disable @typescript-eslint/no-explicit-any */
function rehypeMarkStandaloneLinks() {
  return (tree: any) => {
    const walk = (node: any) => {
      if (!node || !Array.isArray(node.children)) return;
      for (const child of node.children) {
        if (
          child.type === "element" &&
          (child.tagName === "p" || child.tagName === "li")
        ) {
          const meaningful = (child.children ?? []).filter(
            (c: any) => !(c.type === "text" && String(c.value ?? "").trim() === "")
          );
          if (
            meaningful.length === 1 &&
            meaningful[0].type === "element" &&
            meaningful[0].tagName === "a"
          ) {
            const link = meaningful[0];
            link.properties = link.properties ?? {};
            const existing = link.properties.className;
            const classes = Array.isArray(existing)
              ? existing.slice()
              : typeof existing === "string"
                ? [existing]
                : [];
            classes.push(STANDALONE_LINK_CLASS);
            link.properties.className = classes;
          }
        }
        walk(child);
      }
    };
    walk(tree);
  };
}
/* eslint-enable @typescript-eslint/no-explicit-any */

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

/**
 * Turn a failed `/api/fs/*` response into a short user-facing message.
 * The backend currently returns 404 for both "workspace not found" and
 * "file not found"; we read the body so the UI can distinguish them.
 */
async function describeFsError(res: Response): Promise<string> {
  let body = "";
  try {
    body = (await res.text()).trim();
  } catch {
    // ignore — fall back to status-only message
  }
  const lower = body.toLowerCase();
  if (lower.includes("workspace") && lower.includes("not found")) {
    return "Workspace no longer exists";
  }
  if (res.status === 404) return "File not found";
  return `Failed to load (${res.status})`;
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
  const isMarkdown = isMarkdownFile(path);
  const canTextPreview = !isImage && isTextPreviewableFile(path);
  const FileIcon = getFileIcon(path);
  const fileName = path.split("/").pop() || "file";

  // imageUrl initial state is null on purpose — the effect below acquires
  // (and refcounts) the cached URL on mount instead of reading the cache
  // directly here. Reading cache without acquiring would let LRU eviction
  // revoke the URL out from under this component's `<img src>`, which is
  // the bug this refcount fix is closing.
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(isImage);
  const [error, setError] = useState<string | null>(null);
  const [fileSize, setFileSize] = useState<number | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [textLoading, setTextLoading] = useState(canTextPreview);
  const [textError, setTextError] = useState<string | null>(null);
  const [textContent, setTextContent] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Fetch image on mount
  useEffect(() => {
    if (!isImage) return;

    let cancelled = false;
    let acquired = false;

    const cached = acquireCachedImageUrl(resolvedPath);
    if (cached) {
      setImageUrl(cached);
      setLoading(false);
      acquired = true;
      return () => {
        cancelled = true;
        if (acquired) releaseImageUrl(resolvedPath);
      };
    }

    const fetchImage = async () => {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      const downloadUrl = `${API_BASE}/api/fs/download?${params.toString()}`;

      try {
        const res = await fetch(downloadUrl, { headers: { ...authHeader() } });
        if (!res.ok) {
          const msg = await describeFsError(res);
          if (!cancelled) setError(msg);
          if (!cancelled) setLoading(false);
          return;
        }
        const blob = await res.blob();
        if (!cancelled) setFileSize(blob.size);
        const url = URL.createObjectURL(blob);
        if (cancelled) {
          // Component unmounted before fetch landed — don't put this URL
          // into the cache where it would leak; just revoke it directly.
          URL.revokeObjectURL(url);
          return;
        }
        const stored = cacheAndAcquireImageUrl(resolvedPath, url);
        acquired = true;
        setImageUrl(stored);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : "Failed to load");
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    fetchImage();
    return () => {
      cancelled = true;
      if (acquired) releaseImageUrl(resolvedPath);
    };
  }, [isImage, resolvedPath, workspaceId, missionId]);

  // Fetch text preview on mount
  useEffect(() => {
    if (!canTextPreview) return;

    let cancelled = false;
    const fetchText = async () => {
      setTextLoading(true);
      setTextError(null);
      setTextContent(null);

      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);

      try {
        const res = await fetch(`${API_BASE}/api/fs/download?${params.toString()}`, {
          headers: { ...authHeader() },
        });
        if (!res.ok) {
          if (!cancelled) setTextError(await describeFsError(res));
          return;
        }
        const blob = await res.blob();
        const raw = await blob.text();
        if (!cancelled) setFileSize(blob.size);

        const limit = 500_000;
        const finalText =
          raw.length > limit
            ? `${raw.slice(0, limit)}\n\n... (file truncated, too large to preview)`
            : raw;
        if (!cancelled) setTextContent(finalText);
      } catch (err) {
        if (!cancelled) setTextError(err instanceof Error ? err.message : "Failed to load");
      } finally {
        if (!cancelled) setTextLoading(false);
      }
    };

    void fetchText();
    return () => { cancelled = true; };
  }, [canTextPreview, resolvedPath, workspaceId, missionId]);

  // Escape key handler
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleCopy = useCallback(async () => {
    if (!textContent) return;
    try {
      await navigator.clipboard.writeText(textContent);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Ignore; clipboard may be unavailable in some contexts.
    }
  }, [textContent]);

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
      className="fixed inset-0 z-50 flex items-center justify-center p-4 md:p-8"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="absolute inset-0 bg-black/70 backdrop-blur-sm pointer-events-none" />
      <div
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "relative rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl",
          "animate-in fade-in zoom-in-95 duration-200",
          // Previewable files open as a full-page reader (the whole viewport
          // minus padding) so the document clearly sits over the page rather
          // than reading as a chat-width inline expansion.
          isImage || canTextPreview
            ? "flex h-full w-full max-w-6xl flex-col"
            : "max-w-md w-full"
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
          <div className="flex items-center gap-2 shrink-0 ml-3">
            {canTextPreview && textContent && (
              <button
                onClick={handleCopy}
                className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
                title={copied ? "Copied" : "Copy"}
              >
                {copied ? <Check className="h-4 w-4 text-emerald-400" /> : <Copy className="h-4 w-4" />}
              </button>
            )}
            <button
              onClick={onClose}
              className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <div
          className={cn(
            "p-5",
            (isImage || canTextPreview) && "flex min-h-0 flex-1 flex-col"
          )}
        >
          {isImage ? (
            <div className="flex min-h-0 flex-1 flex-col gap-4">
              <div className="relative min-h-[200px] flex-1 rounded-xl overflow-hidden bg-black/20 flex items-center justify-center">
                {loading && (
                  <div className="absolute inset-0 flex flex-col items-center justify-center gap-3">
                    <div className="w-full max-w-[300px] h-[200px] rounded-lg bg-white/[0.03] animate-pulse" />
                    <span className="text-xs text-white/40">Loading preview...</span>
                  </div>
                )}
                {error && !loading && (
                  <div className="flex flex-col items-center justify-center gap-3 py-8">
                    <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-red-500/10">
                      <ImageIcon className="h-6 w-6 text-red-400" />
                    </div>
                    <span className="text-sm text-white/50">{error}</span>
                  </div>
                )}
                {imageUrl && !loading && (
                  /* eslint-disable-next-line @next/next/no-img-element */
                  <img src={imageUrl} alt={fileName} className="max-w-full max-h-full object-contain" />
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
          ) : canTextPreview ? (
            <div className="flex min-h-0 flex-1 flex-col gap-4">
              <div className="relative min-h-0 flex-1 rounded-xl overflow-hidden bg-black/20 border border-white/[0.06]">
                {textLoading && (
                  <div className="p-4">
                    <div className="h-4 w-2/3 rounded bg-white/[0.04] animate-pulse mb-2" />
                    <div className="h-4 w-1/2 rounded bg-white/[0.04] animate-pulse mb-2" />
                    <div className="h-4 w-5/6 rounded bg-white/[0.04] animate-pulse" />
                    <div className="mt-3 text-xs text-white/40">Loading preview...</div>
                  </div>
                )}
                {textError && !textLoading && (
                  <div className="flex flex-col items-center justify-center gap-3 py-8">
                    <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-red-500/10">
                      <FileText className="h-6 w-6 text-red-400" />
                    </div>
                    <span className="text-sm text-white/50">{textError}</span>
                  </div>
                )}
                {textContent != null && !textLoading && (
                  <div className="h-full overflow-auto p-4">
                    {isMarkdown ? (
                      <div className="prose-glass text-sm [&_p]:my-2">
                        <Markdown
                          remarkPlugins={[remarkGfm]}
                          components={{
                            code({ className: codeClassName, children }) {
                              const match = /language-(\w+)/.exec(codeClassName || "");
                              const codeString = String(children).replace(/\n$/, "");
                              const inline = !match && !codeString.includes("\n");
                              if (inline) {
                                return (
                                  <code className="px-1.5 py-0.5 rounded bg-white/[0.06] text-indigo-300 text-xs font-mono">
                                    {children}
                                  </code>
                                );
                              }
                              return (
                                <div className="relative group my-3 rounded-lg overflow-hidden">
                                  <CopyCodeButton code={codeString} />
                                  <LazyCodeBlock
                                    language={match ? match[1] : "markdown"}
                                    customStyle={{
                                      padding: "1rem",
                                      borderRadius: "0.5rem",
                                      background: "rgb(var(--code-background))",
                                    }}
                                  >
                                    {codeString}
                                  </LazyCodeBlock>
                                </div>
                              );
                            },
                            pre({ children }) {
                              return <>{children}</>;
                            },
                          }}
                        >
                          {textContent}
                        </Markdown>
                      </div>
                    ) : (
                      <pre className="whitespace-pre-wrap break-words text-xs font-mono text-white/80 leading-relaxed">
                        {textContent}
                      </pre>
                    )}
                  </div>
                )}
              </div>
              <div className="flex items-center justify-between pt-2 border-t border-white/[0.06]">
                <div className="text-xs text-white/40">
                  {fileSize != null ? formatFileSize(fileSize) : "Text file"}
                  {textContent ? <span className="ml-2">{textContent.split("\n").length} lines</span> : null}
                </div>
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

/** Inline image preview rendered for `<image path="..." />` tags. */
function InlineImagePreview({
  path,
  alt,
  basePath,
  workspaceId,
  missionId,
}: {
  path: string;
  alt: string;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}) {
  const isAbsolute = path.startsWith("/") || /^[a-zA-Z]:/.test(path);
  // A relative path needs `basePath` to resolve. If the parent passed a
  // `workspaceId` but no `basePath`, the workspace lookup itself failed
  // (workspace was deleted/renamed) — the basePath will never arrive, so
  // show a clear error instead of spinning forever. If neither is set, it's
  // pre-mount: keep the loading sentinel and let the effect re-run.
  const resolvedPath = isAbsolute || basePath ? resolvePath(path, basePath) : null;
  const unresolvable = !isAbsolute && !basePath && !!workspaceId;
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Bumped in `onLoad` after recording the image's dimensions so the
  // render below re-reads `imageDimsCache` and swaps the pre-decode
  // reservation for the exact final box.
  const [, setDimsVersion] = useState(0);

  // Layout effect (not useEffect): the cached-URL path is synchronous, so
  // running before paint means a remounted preview (virtualized row
  // scrolled back into view) commits the real `<img>` without ever
  // painting the loading skeleton — the timeline virtualizer never sees
  // the intermediate (shorter) box and rows below don't jump.
  useLayoutEffect(() => {
    // Reset between effect runs so a previous error/blob doesn't leak when
    // `resolvedPath` changes (e.g. `basePath` resolves after first paint).
    setImageUrl(null);
    setError(null);
    setLoading(true);
    if (!resolvedPath) {
      if (unresolvable) {
        setError("Workspace unavailable");
        setLoading(false);
      }
      return;
    }

    let cancelled = false;
    let acquired = false;

    const cached = acquireCachedImageUrl(resolvedPath);
    if (cached) {
      setImageUrl(cached);
      setLoading(false);
      acquired = true;
      return () => {
        cancelled = true;
        if (acquired) releaseImageUrl(resolvedPath);
      };
    }

    const fetchImage = async () => {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      try {
        const res = await fetch(`${API_BASE}/api/fs/download?${params.toString()}`, {
          headers: { ...authHeader() },
        });
        if (!res.ok) {
          const msg = await describeFsError(res);
          if (!cancelled) setError(msg);
          if (!cancelled) setLoading(false);
          return;
        }
        const blob = await res.blob();
        const url = URL.createObjectURL(blob);
        if (cancelled) {
          URL.revokeObjectURL(url);
          return;
        }
        const stored = cacheAndAcquireImageUrl(resolvedPath, url);
        acquired = true;
        setImageUrl(stored);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : "Failed to load");
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    fetchImage();
    return () => {
      cancelled = true;
      if (acquired) releaseImageUrl(resolvedPath);
    };
  }, [resolvedPath, unresolvable, workspaceId, missionId]);

  // Shared placeholder box: same shape for loading and error so the layout
  // doesn't jump and the error state reads as part of the same chrome as
  // the skeleton. `<span>` (not `<div>`) keeps this valid inside the `<p>`
  // that react-markdown wraps around `![alt](url)` so hydration doesn't
  // tear the subtree.
  //
  // When this image has rendered before, reserve its exact laid-out box so
  // the skeleton → image swap is layout-neutral (critical inside the
  // virtualized timeline — see `imageDimsCache`). Otherwise default to
  // 300px: `max-h-[300px]` makes that the final height for any screenshot
  // taller than 300px, which is the overwhelmingly common case, so the
  // first-load shift is usually zero instead of +100px per image.
  const knownDims = resolvedPath ? imageDimsCache.get(resolvedPath) : undefined;
  const placeholderClass =
    "my-2 block rounded-xl overflow-hidden border border-white/[0.06] bg-white/[0.03]";
  const placeholderStyle = knownDims
    ? { width: knownDims.width, height: knownDims.height, maxWidth: "100%" as const }
    : ({ maxWidth: 400, height: 300 } as const);

  if (error) {
    return (
      <span
        className={cn(placeholderClass, "flex items-center justify-center gap-2 text-xs text-white/40")}
        style={placeholderStyle}
        title={error}
      >
        <ImageIcon className="h-4 w-4" />
        <span className="truncate max-w-[260px]">{error}</span>
      </span>
    );
  }

  if (loading || !imageUrl) {
    return (
      <span className={cn(placeholderClass, "animate-pulse")} style={placeholderStyle} />
    );
  }

  return (
    <span className="my-2 block">
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={imageUrl}
        alt={alt}
        className="max-h-[300px] rounded-xl border border-white/[0.06] cursor-pointer hover:border-white/[0.12] transition-colors"
        // Pin the box to the remembered size so the row's height is stable
        // from the very first commit, even before the blob is decoded
        // (an `<img>` has no intrinsic size until decode completes, which
        // is async even for cached blob URLs). Without remembered dims —
        // e.g. the URL is cached but the row unmounted before `onLoad`
        // ever fired — reserve the 300px fallback height instead of
        // letting the box collapse to its (zero) pre-decode size; `onLoad`
        // then records the real dims and re-renders with the exact box.
        style={
          knownDims
            ? { width: knownDims.width, height: knownDims.height, maxWidth: "100%" }
            : { height: 300, maxWidth: "100%" }
        }
        onLoad={(e) => {
          const img = e.currentTarget;
          // Derive the final box from the intrinsic size and the
          // max-h-[300px] rule rather than offsetWidth/offsetHeight: the
          // pre-decode `height: 300` pin is still applied at this point,
          // so the laid-out box of a shorter image would read as
          // (stretched) 300px and get cached wrong.
          if (resolvedPath && img.naturalWidth > 0 && img.naturalHeight > 0) {
            const height = Math.min(300, img.naturalHeight);
            const width = Math.round((img.naturalWidth / img.naturalHeight) * height);
            imageDimsCache.set(resolvedPath, { width, height });
            setDimsVersion((v) => v + 1);
          }
        }}
        onClick={() => showFilePreviewModal(path, resolvedPath ?? path, workspaceId, missionId)}
      />
    </span>
  );
}

/** Inline file download card rendered for `<file path="..." />` tags. */
function InlineFileCard({
  path,
  displayName,
  basePath,
  workspaceId,
  missionId,
}: {
  path: string;
  displayName: string;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}) {
  const isAbsolute = path.startsWith("/") || /^[a-zA-Z]:/.test(path);
  const canResolve = isAbsolute || !!basePath;
  // Same logic as InlineImagePreview: a relative path with no basePath but a
  // workspaceId means the workspace lookup failed — surface that as its own
  // error instead of "File not found".
  const unresolvable = !canResolve && !!workspaceId;
  const resolvedPath = canResolve ? resolvePath(path, basePath) : path;
  const FileIcon = getFileIcon(path);
  const ext = path.split(".").pop()?.toUpperCase() || "";
  const [metadata, setMetadata] = useState<{
    size?: number;
    exists: boolean;
    errorMessage?: string;
  } | null>(null);
  const [downloading, setDownloading] = useState(false);

  useEffect(() => {
    if (unresolvable) {
      setMetadata({ exists: false, errorMessage: "Workspace unavailable" });
      return;
    }
    if (!canResolve) {
      // Pre-mount: basePath may still arrive. Keep skeleton.
      setMetadata(null);
      return;
    }
    let cancelled = false;
    const fetchMeta = async () => {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      try {
        const res = await fetch(`${API_BASE}/api/fs/validate?${params.toString()}`, {
          headers: { ...authHeader() },
        });
        if (res.ok) {
          const data = await res.json();
          if (!cancelled) setMetadata({ exists: data.exists, size: data.size });
        } else {
          const msg = await describeFsError(res);
          if (!cancelled) setMetadata({ exists: false, errorMessage: msg });
        }
      } catch {
        if (!cancelled) setMetadata({ exists: false });
      }
    };
    fetchMeta();
    return () => { cancelled = true; };
  }, [resolvedPath, workspaceId, missionId, canResolve, unresolvable]);

  const handleDownload = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setDownloading(true);
    try {
      const API_BASE = getRuntimeApiBase();
      const params = new URLSearchParams({ path: resolvedPath });
      if (workspaceId) params.set("workspace_id", workspaceId);
      if (missionId) params.set("mission_id", missionId);
      const res = await fetch(`${API_BASE}/api/fs/download?${params.toString()}`, {
        headers: { ...authHeader() },
      });
      if (!res.ok) return;
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = displayName;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } finally {
      setDownloading(false);
    }
  };

  if (metadata && !metadata.exists) {
    const label = metadata.errorMessage
      ? `${metadata.errorMessage}: ${displayName}`
      : `File not found: ${displayName}`;
    return (
      <span className="inline-flex items-center gap-1.5 px-2 py-1 rounded-md bg-red-500/10 text-red-400 text-xs">
        <File className="h-3.5 w-3.5" />
        {label}
      </span>
    );
  }

  return (
    <span
      className={cn(
        "my-2 inline-flex items-center gap-3 px-4 py-3 rounded-xl",
        "bg-white/[0.04] border border-white/[0.06] hover:bg-white/[0.06] hover:border-white/[0.1]",
        "cursor-pointer transition-colors max-w-sm"
      )}
      onClick={() => showFilePreviewModal(path, resolvedPath, workspaceId, missionId)}
    >
      <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-indigo-500/10">
        <FileIcon className="h-4 w-4 text-indigo-400" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-white/80 truncate">
          {displayName}
        </span>
        <span className="block text-xs text-white/40">
          {ext && <span className="mr-2">{ext}</span>}
          {metadata?.size != null && <span>{formatFileSize(metadata.size)}</span>}
        </span>
      </span>
      <button
        onClick={handleDownload}
        disabled={downloading}
        className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors shrink-0"
        title="Download"
      >
        <Download className={cn("h-4 w-4", downloading && "animate-pulse")} />
      </button>
    </span>
  );
}

// Compact, text-flowing reference for a file linked mid-prose. Unlike
// InlineFileCard this stays on the baseline and does no metadata fetch — it is
// just a clickable filename that opens the preview modal.
function InlineFileChip({
  path,
  displayName,
  basePath,
  workspaceId,
  missionId,
}: {
  path: string;
  displayName: string;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}) {
  const isAbsolute = path.startsWith("/") || /^[a-zA-Z]:/.test(path);
  const canResolve = isAbsolute || !!basePath;
  const resolvedPath = canResolve ? resolvePath(path, basePath) : path;
  const FileIcon = getFileIcon(path);
  return (
    <button
      type="button"
      title="Click to preview"
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        showFilePreviewModal(path, resolvedPath, workspaceId, missionId);
      }}
      className={cn(
        "inline-flex items-center gap-1 align-middle rounded px-1.5 py-0.5",
        "bg-indigo-500/[0.12] text-indigo-300 hover:bg-indigo-500/20 hover:text-indigo-200",
        "font-mono text-[0.85em] leading-none cursor-pointer transition-colors"
      )}
    >
      {/* getFileIcon returns a stable module-level lucide component, not a new one. */}
      {/* eslint-disable-next-line react-hooks/static-components */}
      <FileIcon className="h-3 w-3 shrink-0 opacity-70" />
      <span className="truncate max-w-[16rem]">{displayName}</span>
    </button>
  );
}

// P1-#10: messages larger than this render as plain <pre> with an opt-in
// "Render markdown" button. The freeze trace on the verity missions
// showed single assistant bubbles 200KB+ of repeated tokens that took
// 5s+ to highlight + lay out. Past ~50KB the cost is no longer paying
// for anything the user actually reads.
const MARKDOWN_SIZE_CAP_BYTES = 50_000;

// Memoized to prevent re-renders when parent re-renders with same props
export const MarkdownContent = memo(function MarkdownContent({
  content,
  className,
  basePath,
  workspaceId,
  missionId,
  missionLinks,
}: MarkdownContentProps) {
  const [forceMarkdown, setForceMarkdown] = useState(false);
  const oversize = content.length > MARKDOWN_SIZE_CAP_BYTES;
  const renderPlain = oversize && !forceMarkdown;

  // Pre-process content: transform <image> and <file> tags into markdown syntax
  const processedContent = useMemo(
    () => (renderPlain ? "" : transformRichTags(content)),
    [content, renderPlain]
  );

  // Memoize components object to prevent react-markdown from re-creating DOM on every render
  const components: Components = useMemo(() => ({
    img({ src, alt, ...props }) {
      // Handle sandboxed-image:// protocol for rich image tags
      const srcStr = typeof src === "string" ? src : undefined;
      if (srcStr?.startsWith("sandboxed-image://")) {
        const path = decodeURIComponent(srcStr.replace("sandboxed-image://", ""));
        return (
          <InlineImagePreview
            path={path}
            alt={alt || path}
            basePath={basePath}
            workspaceId={workspaceId}
            missionId={missionId}
          />
        );
      }
      // Default img rendering
      // eslint-disable-next-line @next/next/no-img-element
      return <img src={srcStr} alt={alt} {...props} className="max-w-full rounded" />;
    },
    a({ href, children, className, ...props }) {
      if (href?.startsWith("mission://")) {
        return <MissionChip missionId={href.replace("mission://", "")} />;
      }
      if (href && isRichFileLinkHref(href)) {
        const path = href.startsWith("sandboxed-file://")
          ? decodeURIComponent(href.replace("sandboxed-file://", ""))
          : decodeURIComponent(href.split(/[?#]/, 1)[0]);
        const childText = Array.isArray(children) ? children.join("") : String(children || "");
        const displayName = childText || path.split("/").pop() || "file";
        const standalone =
          typeof className === "string" && className.includes(STANDALONE_LINK_CLASS);
        // Standalone (own paragraph/list-item) → full download card; a file
        // mentioned mid-prose → compact inline chip so text keeps flowing.
        return standalone ? (
          <InlineFileCard
            path={path}
            displayName={displayName}
            basePath={basePath}
            workspaceId={workspaceId}
            missionId={missionId}
          />
        ) : (
          <InlineFileChip
            path={path}
            displayName={displayName}
            basePath={basePath}
            workspaceId={workspaceId}
            missionId={missionId}
          />
        );
      }
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
                "code-inline text-xs font-mono",
                "cursor-pointer transition-colors"
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
          <code className="code-inline text-xs font-mono" {...props}>
            {children}
          </code>
        );
      }

      return (
        <div className="relative group my-3 rounded-lg overflow-hidden">
          <CopyCodeButton code={codeString} />
          {match ? (
            <LazyCodeBlock
              language={match[1]}
              customStyle={{
                padding: "1rem",
                borderRadius: "0.5rem",
                background: "rgb(var(--code-background))",
              }}
            >
              {codeString}
            </LazyCodeBlock>
          ) : (
            <pre className="code-block p-4 overflow-x-auto">
              <code className="text-xs font-mono">{codeString}</code>
            </pre>
          )}
          {match && (
            <div className="absolute left-3 top-2 text-[10px] muted-text uppercase tracking-wider">{match[1]}</div>
          )}
        </div>
      );
    },
    pre({ children }) {
      return <>{children}</>;
    },
  }), [basePath, workspaceId, missionId]);

  // Memoize remarkPlugins array to prevent recreation
  const plugins = useMemo(
    () => (missionLinks ? [remarkGfm, remarkMissionLinks] : [remarkGfm]),
    [missionLinks],
  );
  const rehypePlugins = useMemo(() => [rehypeMarkStandaloneLinks], []);

  // Allow our placeholder protocols through react-markdown's URL sanitizer.
  // Everything else should continue to use the default sanitizer behavior.
  const urlTransform = useCallback((url: string) => {
    if (
      url.startsWith("sandboxed-image://") ||
      url.startsWith("sandboxed-file://") ||
      url.startsWith("mission://")
    ) {
      return url;
    }
    return defaultUrlTransform(url);
  }, []);

  if (renderPlain) {
    const sizeKb = (content.length / 1024).toFixed(0);
    return (
      <div className={cn("prose-glass text-sm [&_p]:my-2", className)}>
        <div className="mb-2 flex items-center justify-between rounded border border-amber-400/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          <span>
            Large message ({sizeKb} KB). Markdown rendering skipped for
            performance — code blocks, links, and images are not active.
          </span>
          <button
            type="button"
            onClick={() => setForceMarkdown(true)}
            className="ml-3 shrink-0 rounded bg-amber-400/20 px-2 py-0.5 text-xs font-medium text-amber-100 hover:bg-amber-400/30"
          >
            Render markdown
          </button>
        </div>
        <pre className="code-block max-h-[60vh] overflow-auto whitespace-pre-wrap break-words p-3 text-xs leading-relaxed">
          {content}
        </pre>
      </div>
    );
  }

  return (
    <div className={cn("prose-glass text-sm [&_p]:my-2", className)}>
      <Markdown remarkPlugins={plugins} rehypePlugins={rehypePlugins} components={components} urlTransform={urlTransform}>
        {processedContent}
      </Markdown>
    </div>
  );
});

/**
 * P2-#13: lazy-mount wrapper around `MarkdownContent`.
 *
 * Renders the raw text inside a `<pre>` until the first IntersectionObserver
 * hit, then upgrades to the full markdown pipeline. One-way upgrade: once
 * a bubble has been seen we keep the rich renderer mounted so scroll-out
 * doesn't unmount + remount the syntax-highlighted code blocks.
 *
 * Bubbles smaller than the threshold skip the lazy path entirely — the
 * setup cost of an IO observer + the placeholder swap isn't worth it for
 * a 100-char ack message. Threshold was 1 KB but raised to 5 KB after we
 * disabled tanstack-virtual's resize-driven scroll compensation: the
 * placeholder→markdown swap changes bubble height, and without virtualizer
 * adjustment, that delta now shifts visible content under the user's
 * reading position when they scroll into history. Most assistant messages
 * are under 5 KB; only large summaries take the lazy path.
 */
const LAZY_THRESHOLD_BYTES = 5_000;

export function LazyMarkdownContent(props: MarkdownContentProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const small = props.content.length < LAZY_THRESHOLD_BYTES;
  const [visible, setVisible] = useState(small);

  useEffect(() => {
    if (visible) return;
    const node = ref.current;
    if (!node) return;
    if (typeof IntersectionObserver === "undefined") {
      // Older browser — just upgrade immediately. CSS content-visibility
      // already provides the bulk of the win on the chat list.
      const timer = window.setTimeout(() => setVisible(true), 0);
      return () => window.clearTimeout(timer);
    }
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            setVisible(true);
            observer.disconnect();
            break;
          }
        }
      },
      // Generous rootMargin so the upgrade fires well before the bubble
      // scrolls into the viewport. The chat virtualizer keeps ~8 items
      // of overscan (~1500-2500 px); covering that range means the
      // placeholder→markdown swap happens on mount in overscan, never
      // while the bubble is visually present. Without this, scrolling
      // suddenly (scrollbar click, keyboard PgUp) lands the user on a
      // bubble that then upgrades and shifts every item below it.
      { rootMargin: "1200px" }
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [visible]);

  if (visible) {
    return <MarkdownContent {...props} />;
  }

  // Placeholder: raw text in a pre block. Reserves a similar vertical
  // footprint to the rendered markdown so scroll position stays stable
  // when the upgrade swaps the children.
  return (
    <div
      ref={ref}
      className={cn("prose-glass text-sm [&_p]:my-2", props.className)}
    >
      <pre className="code-block whitespace-pre-wrap break-words text-sm leading-relaxed">
        {props.content}
      </pre>
    </div>
  );
}
