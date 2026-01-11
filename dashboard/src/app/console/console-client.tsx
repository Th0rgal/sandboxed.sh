"use client";

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { Terminal as XTerm } from "xterm";
import { FitAddon } from "xterm-addon-fit";
import { toast } from "@/components/toast";
import "xterm/css/xterm.css";

import { authHeader, getValidJwt } from "@/lib/auth";
import { getRuntimeApiBase } from "@/lib/settings";
import { CopyButton } from "@/components/ui/copy-button";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";

const isTerminalDebugEnabled = () => {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem("openagent.debug.terminal") === "1";
};

function terminalDebug(...args: unknown[]) {
  if (!isTerminalDebugEnabled()) return;
  // eslint-disable-next-line no-console
  console.debug("[terminal]", ...args);
}

type WsLogLevel = "debug" | "info" | "warn" | "error";

function wsLog(level: WsLogLevel, message: string, meta?: Record<string, unknown>) {
  const prefix = "[console:ws]";
  const args = meta ? [prefix, message, meta] : [prefix, message];
  switch (level) {
    case "debug":
      // eslint-disable-next-line no-console
      console.debug(...args);
      break;
    case "info":
      // eslint-disable-next-line no-console
      console.info(...args);
      break;
    case "warn":
      // eslint-disable-next-line no-console
      console.warn(...args);
      break;
    case "error":
      // eslint-disable-next-line no-console
      console.error(...args);
      break;
  }
}

type FsEntry = {
  name: string;
  path: string;
  kind: "file" | "dir" | "link" | "other" | string;
  size: number;
  mtime: number;
};

type TabType = "terminal" | "files" | "workspace-shell";

type Tab = {
  id: string;
  type: TabType;
  title: string;
  // For workspace-shell tabs
  workspaceId?: string;
  workspaceName?: string;
};

function formatBytes(n: number) {
  if (!Number.isFinite(n)) return "-";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"] as const;
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 10 ? 0 : 1)} ${units[i]}`;
}

async function listDir(path: string): Promise<FsEntry[]> {
  const API_BASE = getRuntimeApiBase();
  const res = await fetch(
    `${API_BASE}/api/fs/list?path=${encodeURIComponent(path)}`,
    {
      headers: { ...authHeader() },
    }
  );
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

async function mkdir(path: string): Promise<void> {
  const API_BASE = getRuntimeApiBase();
  const res = await fetch(`${API_BASE}/api/fs/mkdir`, {
    method: "POST",
    headers: { "Content-Type": "application/json", ...authHeader() },
    body: JSON.stringify({ path }),
  });
  if (!res.ok) throw new Error(await res.text());
}

async function rm(path: string, recursive = false): Promise<void> {
  const API_BASE = getRuntimeApiBase();
  const res = await fetch(`${API_BASE}/api/fs/rm`, {
    method: "POST",
    headers: { "Content-Type": "application/json", ...authHeader() },
    body: JSON.stringify({ path, recursive }),
  });
  if (!res.ok) throw new Error(await res.text());
}

async function downloadFile(path: string) {
  const API_BASE = getRuntimeApiBase();
  const res = await fetch(
    `${API_BASE}/api/fs/download?path=${encodeURIComponent(path)}`,
    {
      headers: { ...authHeader() },
    }
  );
  if (!res.ok) throw new Error(await res.text());
  const blob = await res.blob();
  const name = path.split("/").filter(Boolean).pop() ?? "download";
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

async function fetchFileContent(path: string): Promise<string> {
  const API_BASE = getRuntimeApiBase();
  const res = await fetch(
    `${API_BASE}/api/fs/download?path=${encodeURIComponent(path)}`,
    {
      headers: { ...authHeader() },
    }
  );
  if (!res.ok) throw new Error(await res.text());
  const text = await res.text();
  return text;
}

// Get language from file extension for syntax highlighting
function getLanguageFromPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const languageMap: Record<string, string> = {
    js: "javascript",
    jsx: "jsx",
    ts: "typescript",
    tsx: "tsx",
    py: "python",
    rb: "ruby",
    rs: "rust",
    go: "go",
    java: "java",
    kt: "kotlin",
    swift: "swift",
    c: "c",
    cpp: "cpp",
    h: "c",
    hpp: "cpp",
    cs: "csharp",
    php: "php",
    sh: "bash",
    bash: "bash",
    zsh: "bash",
    fish: "bash",
    ps1: "powershell",
    sql: "sql",
    json: "json",
    yaml: "yaml",
    yml: "yaml",
    toml: "toml",
    xml: "xml",
    html: "html",
    htm: "html",
    css: "css",
    scss: "scss",
    sass: "sass",
    less: "less",
    md: "markdown",
    markdown: "markdown",
    dockerfile: "dockerfile",
    makefile: "makefile",
    cmake: "cmake",
    gitignore: "gitignore",
    env: "bash",
    ini: "ini",
    conf: "ini",
    cfg: "ini",
    tex: "latex",
    r: "r",
    R: "r",
    scala: "scala",
    lua: "lua",
    vim: "vim",
    graphql: "graphql",
    proto: "protobuf",
  };
  return languageMap[ext] || "text";
}

// Check if a file is likely text based on extension
function isTextFile(path: string): boolean {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const textExts = new Set([
    "txt", "md", "markdown", "json", "yaml", "yml", "toml", "xml", "html", "htm",
    "css", "scss", "sass", "less", "js", "jsx", "ts", "tsx", "py", "rb", "rs", "go",
    "java", "kt", "swift", "c", "cpp", "h", "hpp", "cs", "php", "sh", "bash", "zsh",
    "fish", "ps1", "sql", "dockerfile", "makefile", "cmake", "gitignore", "env",
    "ini", "conf", "cfg", "tex", "r", "scala", "lua", "vim", "graphql", "proto",
    "log", "csv", "tsv", "lock", "editorconfig", "prettierrc", "eslintrc",
  ]);
  // Also check for files without extension that are commonly text
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  const textNames = new Set([
    "dockerfile", "makefile", "readme", "license", "changelog", "authors",
    "contributing", "todo", "gitignore", "dockerignore", "eslintignore",
  ]);
  return textExts.has(ext) || textNames.has(name);
}

// Check if a file is an image
function isImageFile(path: string): boolean {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"].includes(ext);
}

// File Preview Modal Component
function FilePreviewModal({
  path,
  onClose,
}: {
  path: string;
  onClose: () => void;
}) {
  const [content, setContent] = useState<string | null>(null);
  const [imageBlobUrl, setImageBlobUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const blobUrlRef = useRef<string | null>(null);
  const fileName = path.split("/").pop() ?? "file";
  const language = getLanguageFromPath(path);
  const isImage = isImageFile(path);

  // Handle Escape key to close modal
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  useEffect(() => {
    let isStale = false;

    // Revoke previous blob URL if any
    if (blobUrlRef.current) {
      URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = null;
    }

    async function loadFile() {
      setLoading(true);
      setError(null);
      setImageBlobUrl(null);
      try {
        if (isImage) {
          // Fetch image as blob with authentication
          const API_BASE = getRuntimeApiBase();
          const res = await fetch(
            `${API_BASE}/api/fs/download?path=${encodeURIComponent(path)}`,
            {
              headers: { ...authHeader() },
            }
          );
          if (!res.ok) throw new Error(await res.text());
          const blob = await res.blob();
          const blobUrl = URL.createObjectURL(blob);
          // Check if effect was cleaned up while fetch was in-flight
          if (isStale) {
            URL.revokeObjectURL(blobUrl);
            return;
          }
          blobUrlRef.current = blobUrl;
          setImageBlobUrl(blobUrl);
          setContent("image");
        } else {
          const text = await fetchFileContent(path);
          if (isStale) return;
          // Limit preview size
          if (text.length > 500000) {
            setContent(text.slice(0, 500000) + "\n\n... (file truncated, too large to preview)");
          } else {
            setContent(text);
          }
        }
      } catch (err) {
        if (isStale) return;
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (!isStale) {
          setLoading(false);
        }
      }
    }
    void loadFile();

    // Cleanup blob URL on unmount or path change
    return () => {
      isStale = true;
      if (blobUrlRef.current) {
        URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = null;
      }
    };
  }, [path, isImage]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="relative max-h-[85vh] max-w-[90vw] w-full max-w-4xl flex flex-col rounded-xl border border-[var(--border)] bg-[var(--background-secondary)] shadow-2xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-3">
          <div className="flex items-center gap-3">
            <span className="text-lg">{isImage ? "🖼️" : "📄"}</span>
            <div>
              <h3 className="font-medium text-[var(--foreground)]">{fileName}</h3>
              <p className="text-xs text-[var(--foreground-muted)]">{path}</p>
            </div>
          </div>
          <div className="flex items-center gap-2">
            {!isImage && content && (
              <CopyButton text={content} className="h-8 w-8" />
            )}
            <button
              className="flex h-8 items-center gap-1.5 rounded-md border border-[var(--border)] bg-[var(--background-tertiary)] px-3 text-xs text-[var(--foreground-muted)] hover:text-[var(--foreground)]"
              onClick={() => void downloadFile(path)}
            >
              <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
              </svg>
              Download
            </button>
            <button
              className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--foreground-muted)] hover:bg-[var(--background-tertiary)] hover:text-[var(--foreground)]"
              onClick={onClose}
            >
              <svg className="h-5 w-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
              </svg>
            </button>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto min-h-0">
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <svg className="h-6 w-6 animate-spin text-[var(--foreground-muted)]" fill="none" viewBox="0 0 24 24">
                <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
              </svg>
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-16 text-red-400">
              <svg className="h-8 w-8 mb-2" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
              </svg>
              <p className="text-sm">{error}</p>
            </div>
          ) : isImage && imageBlobUrl ? (
            <div className="flex items-center justify-center p-8 bg-[var(--background)]">
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={imageBlobUrl}
                alt={fileName}
                className="max-h-[60vh] max-w-full object-contain"
              />
            </div>
          ) : (
            <div className="text-sm">
              <SyntaxHighlighter
                language={language}
                style={oneDark}
                showLineNumbers
                customStyle={{
                  margin: 0,
                  padding: "1rem",
                  background: "transparent",
                  fontSize: "0.8125rem",
                }}
                codeTagProps={{
                  style: {
                    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
                  },
                }}
              >
                {content || ""}
              </SyntaxHighlighter>
            </div>
          )}
        </div>

        {/* Footer */}
        {!loading && !error && !isImage && content && (
          <div className="border-t border-[var(--border)] px-4 py-2 text-xs text-[var(--foreground-muted)] flex items-center justify-between">
            <span>{content.split("\n").length} lines</span>
            <span className="uppercase tracking-wider">{language}</span>
          </div>
        )}
      </div>
    </div>
  );
}

async function uploadFiles(
  dir: string,
  files: File[],
  onProgress?: (done: number, total: number) => void
) {
  let done = 0;
  for (const f of files) {
    await new Promise<void>((resolve, reject) => {
      const API_BASE = getRuntimeApiBase();
      const form = new FormData();
      form.append("file", f, f.name);
      const xhr = new XMLHttpRequest();
      xhr.open(
        "POST",
        `${API_BASE}/api/fs/upload?path=${encodeURIComponent(dir)}`,
        true
      );
      const jwt = getValidJwt()?.token;
      if (jwt) xhr.setRequestHeader("Authorization", `Bearer ${jwt}`);
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) resolve();
        else
          reject(
            new Error(xhr.responseText || `Upload failed (${xhr.status})`)
          );
      };
      xhr.onerror = () => reject(new Error("Upload failed (network error)"));
      xhr.send(form);
    });
    done += 1;
    onProgress?.(done, files.length);
  }
}

// Generate unique IDs
let tabIdCounter = 0;
function generateTabId(): string {
  return `tab-${++tabIdCounter}-${Date.now()}`;
}

// Terminal Tab Component
function TerminalTab({ tabId, isActive, onStatusChange }: { tabId: string; isActive: boolean; onStatusChange?: (status: "disconnected" | "connecting" | "connected" | "error", reconnect: () => void, reset: () => void) => void }) {
  const termElRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  // Monotonically increasing counter to ignore stale websocket events.
  const wsSeqRef = useRef(0);
  const messageCountRef = useRef(0);
  const retryCountRef = useRef(0);
  const rafOpenRef = useRef<number | null>(null);
  const rafFitRef = useRef<number | null>(null);
  const mountedRef = useRef(true);
  const terminalInitializedRef = useRef(false);
  const [wsStatus, setWsStatus] = useState<
    "disconnected" | "connecting" | "connected" | "error"
  >("disconnected");

  // Helper to create WebSocket connection
  const connectWebSocket = useCallback((term: XTerm, fit: FitAddon, isReconnect = false) => {
    // Invalidate any in-flight websocket callbacks.
    wsSeqRef.current += 1;
    const seq = wsSeqRef.current;
    messageCountRef.current = 0;

    // Close existing WebSocket if any (and detach handlers so it can't write stale output)
    const prev = wsRef.current;
    if (prev && !isReconnect && (prev.readyState === WebSocket.CONNECTING || prev.readyState === WebSocket.OPEN)) {
      terminalDebug("ws already active; skipping connect", { tabId });
      return prev;
    }
    if (prev) {
      terminalDebug("replacing websocket", { tabId, isReconnect });
      try {
        prev.onopen = null;
        prev.onmessage = null;
        prev.onerror = null;
        prev.onclose = null;
      } catch {
        /* ignore */
      }
      try {
        prev.close();
      } catch {
        /* ignore */
      }
    }
    
    setWsStatus("connecting");
    const jwt = getValidJwt()?.token ?? null;
    const proto = jwt
      ? (["openagent", `jwt.${jwt}`] as string[])
      : (["openagent"] as string[]);
    const API_BASE = getRuntimeApiBase();
    const u = new URL(`${API_BASE}/api/console/ws`);
    u.protocol = u.protocol === "https:" ? "wss:" : "ws:";

    let didOpen = false;
    const ws = new WebSocket(u.toString(), proto);
    wsLog("info", "connect", { tabId, url: u.toString(), hasJwt: Boolean(jwt), isReconnect });
    terminalDebug("ws connect", { tabId, url: u.toString(), hasJwt: Boolean(jwt), isReconnect });
    wsRef.current = ws;

    ws.onopen = () => {
      if (!mountedRef.current || wsSeqRef.current !== seq) return;
      didOpen = true;
      retryCountRef.current = 0;
      setWsStatus("connected");
      wsLog("info", "open", { tabId });
      terminalDebug("ws open", { tabId });
      // Fit and send dimensions immediately after connection
      setTimeout(() => {
        if (!mountedRef.current || wsSeqRef.current !== seq) return;
        try {
          fit.fit();
          ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
        } catch { /* ignore */ }
      }, 50);
      // If we didn't get any output, nudge the shell to redraw a prompt.
      setTimeout(() => {
        if (!mountedRef.current || wsSeqRef.current !== seq) return;
        if (messageCountRef.current === 0 && ws.readyState === WebSocket.OPEN) {
          try {
            ws.send(JSON.stringify({ t: "i", d: "\r" }));
            terminalDebug("sent prompt nudge", { tabId });
          } catch { /* ignore */ }
        }
      }, 300);
    };
    ws.onmessage = (evt) => {
      if (!mountedRef.current || wsSeqRef.current !== seq) return;
      messageCountRef.current += 1;
      if (isTerminalDebugEnabled() && messageCountRef.current <= 3) {
        wsLog("debug", "message", {
          tabId,
          bytes: typeof evt.data === "string" ? evt.data.length : 0,
        });
        terminalDebug("ws message", {
          tabId,
          bytes: typeof evt.data === "string" ? evt.data.length : 0,
        });
      }
      term.write(typeof evt.data === "string" ? evt.data : "");
    };
    ws.onerror = () => {
      if (mountedRef.current && wsSeqRef.current === seq) {
        setWsStatus("error");
        wsLog("error", "error", { tabId });
        terminalDebug("ws error", { tabId });
      }
    };
    ws.onclose = (e) => {
      if (mountedRef.current && wsSeqRef.current === seq) {
        setWsStatus("disconnected");
        wsLog("warn", "close", { tabId, code: e.code, reason: e.reason, wasClean: e.wasClean });
        terminalDebug("ws close", { tabId, code: e.code, reason: e.reason, wasClean: e.wasClean });
        // Only show error for unexpected closures, not normal disconnects
        if (e.code === 1006 && !didOpen) {
          term.writeln("\x1b[90mConnection failed. Check that the console backend is reachable.\x1b[0m");
          if (retryCountRef.current < 1) {
            retryCountRef.current += 1;
            setTimeout(() => {
              if (!mountedRef.current || wsSeqRef.current !== seq) return;
              connectWebSocket(term, fit, true);
            }, 300);
          }
        } else if (e.code !== 1000 && e.code !== 1001 && didOpen) {
          term.writeln("\x1b[90mDisconnected.\x1b[0m");
        }
      }
    };
    
    return ws;
  }, []);

  // Initialize terminal (only once per tab instance)
  useEffect(() => {
    mountedRef.current = true;
    
    // Only init terminal structure once, but connect when active
    if (!isActive) return;
    
    const container = termElRef.current;
    if (!container) return;

    // Create terminal if not already created
    if (!terminalInitializedRef.current) {
      terminalInitializedRef.current = true;
      
      const term = new XTerm({
        fontFamily:
          '"JetBrainsMono Nerd Font Mono", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
        fontSize: 13,
        lineHeight: 1.25,
        cursorBlink: true,
        convertEol: true,
        allowProposedApi: true,
        theme: {
          background: "#0d0d0d",
        },
      });
      const fit = new FitAddon();
      term.loadAddon(fit);

      termRef.current = term;
      fitRef.current = fit;

      // Defer opening to next frame to ensure container has dimensions
      let cancelled = false;
      rafOpenRef.current = requestAnimationFrame(() => {
        if (cancelled || !mountedRef.current) return;
        if (!mountedRef.current) return;
        try {
          term.open(container);
          rafFitRef.current = requestAnimationFrame(() => {
            if (cancelled || !mountedRef.current) return;
            try {
              fit.fit();
              terminalDebug("terminal fit", { tabId, cols: term.cols, rows: term.rows });
            } catch { /* Ignore fit errors */ }
            // Connect WebSocket after terminal is ready
            connectWebSocket(term, fit, false);
          });
        } catch (err) {
          terminalDebug("terminal open failed", { tabId, error: String(err) });
        }
      });

      // Resize handler
      const onResize = () => {
        if (!mountedRef.current) return;
        try {
          fit.fit();
          const ws = wsRef.current;
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
          }
        } catch { /* Ignore */ }
      };
      window.addEventListener("resize", onResize);

      // Forward terminal input to WebSocket
      const onDataDisposable = term.onData((d) => {
        const ws = wsRef.current;
        if (ws?.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ t: "i", d }));
        }
      });

      // Cleanup on unmount
      return () => {
        mountedRef.current = false;
        cancelled = true;
        if (rafOpenRef.current !== null) {
          cancelAnimationFrame(rafOpenRef.current);
          rafOpenRef.current = null;
        }
        if (rafFitRef.current !== null) {
          cancelAnimationFrame(rafFitRef.current);
          rafFitRef.current = null;
        }
        // Invalidate websocket callbacks for this terminal instance.
        wsSeqRef.current += 1;
        window.removeEventListener("resize", onResize);
        try { onDataDisposable.dispose(); } catch { /* ignore */ }
        const ws = wsRef.current;
        if (ws) {
          try {
            ws.onopen = null;
            ws.onmessage = null;
            ws.onerror = null;
            ws.onclose = null;
          } catch {
            /* ignore */
          }
        }
        try { ws?.close(); } catch { /* ignore */ }
        try { term.dispose(); } catch { /* ignore */ }
        wsRef.current = null;
        termRef.current = null;
        fitRef.current = null;
        terminalInitializedRef.current = false;
      };
    }
  }, [isActive, connectWebSocket]);

  // Reconnect function
  const reconnect = useCallback(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) {
      // Terminal not ready yet, nothing to reconnect
      return;
    }
    connectWebSocket(term, fit, true);
  }, [connectWebSocket]);

  const reset = useCallback(() => {
    const ws = wsRef.current;
    if (ws?.readyState === WebSocket.OPEN) {
      try {
        ws.send(JSON.stringify({ t: "i", d: "reset\n" }));
        setTimeout(() => {
          try {
            ws.send(JSON.stringify({ t: "i", d: "stty sane\n" }));
          } catch { /* ignore */ }
        }, 50);
      } catch { /* ignore */ }
    } else {
      reconnect();
    }
  }, [reconnect]);

  // Fit terminal when tab becomes active
  useEffect(() => {
    if (isActive && fitRef.current) {
      // Delay fit to allow layout to settle
      const timer = setTimeout(() => {
        try { fitRef.current?.fit(); } catch { /* ignore */ }
      }, 50);
      return () => clearTimeout(timer);
    }
  }, [isActive]);

  // Report status changes to parent
  useEffect(() => {
    if (isActive && onStatusChange) {
      onStatusChange(wsStatus, reconnect, reset);
    }
  }, [wsStatus, reconnect, reset, isActive, onStatusChange]);

  return (
    <div
      className={[
        "absolute inset-0 h-full min-h-0",
        isActive ? "opacity-100" : "pointer-events-none opacity-0",
      ].join(" ")}
      aria-label={`terminal-tab-${tabId}`}
      ref={termElRef}
    />
  );
}

// Workspace Shell Tab Component - Terminal connected to workspace shell
function WorkspaceShellTab({
  tabId,
  isActive,
  workspaceId,
  workspaceName,
  onStatusChange
}: {
  tabId: string;
  isActive: boolean;
  workspaceId: string;
  workspaceName: string;
  onStatusChange?: (status: "disconnected" | "connecting" | "connected" | "error", reconnect: () => void, reset: () => void) => void;
}) {
  const termElRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const wsSeqRef = useRef(0);
  const messageCountRef = useRef(0);
  const retryCountRef = useRef(0);
  const rafOpenRef = useRef<number | null>(null);
  const rafFitRef = useRef<number | null>(null);
  const mountedRef = useRef(true);
  const terminalInitializedRef = useRef(false);
  const [wsStatus, setWsStatus] = useState<
    "disconnected" | "connecting" | "connected" | "error"
  >("disconnected");

  const diagnoseApiReachability = useCallback(async (apiBase: string) => {
    try {
      const controller = new AbortController();
      const timeout = window.setTimeout(() => controller.abort(), 3000);
      const res = await fetch(`${apiBase}/api/health`, {
        method: "GET",
        signal: controller.signal,
      });
      window.clearTimeout(timeout);
      if (!res.ok) {
        return `API reachable but returned ${res.status} from /api/health.`;
      }
      return "API reachable, but the websocket upgrade failed. If you're behind a reverse proxy, make sure it forwards Upgrade/Connection headers for /api/workspaces/*/shell.";
    } catch {
      return `Cannot reach API at ${apiBase}. Check Settings → API URL, or set HOST=0.0.0.0 on the server.`;
    }
  }, []);

  const connectWebSocket = useCallback((term: XTerm, fit: FitAddon, isReconnect = false) => {
    wsSeqRef.current += 1;
    const seq = wsSeqRef.current;
    messageCountRef.current = 0;

    const prev = wsRef.current;
    if (prev && !isReconnect && (prev.readyState === WebSocket.CONNECTING || prev.readyState === WebSocket.OPEN)) {
      terminalDebug("workspace ws already active; skipping connect", { tabId, workspaceId });
      return prev;
    }
    if (prev) {
      try {
        prev.onopen = null;
        prev.onmessage = null;
        prev.onerror = null;
        prev.onclose = null;
      } catch { /* ignore */ }
      try { prev.close(); } catch { /* ignore */ }
    }

    setWsStatus("connecting");
    const jwt = getValidJwt()?.token ?? null;
    const proto = jwt
      ? (["openagent", `jwt.${jwt}`] as string[])
      : (["openagent"] as string[]);
    const API_BASE = getRuntimeApiBase();
    // Connect to workspace-specific shell endpoint
    const u = new URL(`${API_BASE}/api/workspaces/${workspaceId}/shell`);
    u.protocol = u.protocol === "https:" ? "wss:" : "ws:";

    let didOpen = false;
    const ws = new WebSocket(u.toString(), proto);
    wsLog("info", "workspace connect", {
      tabId,
      workspaceId,
      url: u.toString(),
      hasJwt: Boolean(jwt),
      isReconnect,
    });
    terminalDebug("workspace ws connect", { tabId, workspaceId, url: u.toString(), hasJwt: Boolean(jwt), isReconnect });
    wsRef.current = ws;

    ws.onopen = () => {
      if (!mountedRef.current || wsSeqRef.current !== seq) return;
      didOpen = true;
      retryCountRef.current = 0;
      setWsStatus("connected");
      wsLog("info", "workspace open", { tabId, workspaceId });
      terminalDebug("workspace ws open", { tabId, workspaceId });
      setTimeout(() => {
        if (!mountedRef.current || wsSeqRef.current !== seq) return;
        try {
          fit.fit();
          ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
        } catch { /* ignore */ }
      }, 50);
      // If no output arrives, nudge to redraw a prompt.
      setTimeout(() => {
        if (!mountedRef.current || wsSeqRef.current !== seq) return;
        if (messageCountRef.current === 0 && ws.readyState === WebSocket.OPEN) {
          try {
            ws.send(JSON.stringify({ t: "i", d: "\r" }));
            terminalDebug("workspace sent prompt nudge", { tabId, workspaceId });
          } catch { /* ignore */ }
        }
      }, 300);
    };
    ws.onmessage = (evt) => {
      if (!mountedRef.current || wsSeqRef.current !== seq) return;
      messageCountRef.current += 1;
      if (isTerminalDebugEnabled() && messageCountRef.current <= 3) {
        wsLog("debug", "workspace message", {
          tabId,
          workspaceId,
          bytes: typeof evt.data === "string" ? evt.data.length : 0,
        });
        terminalDebug("workspace ws message", {
          tabId,
          workspaceId,
          bytes: typeof evt.data === "string" ? evt.data.length : 0,
        });
      }
      term.write(typeof evt.data === "string" ? evt.data : "");
    };
    ws.onerror = () => {
      if (mountedRef.current && wsSeqRef.current === seq) {
        setWsStatus("error");
        wsLog("error", "workspace error", { tabId, workspaceId });
        terminalDebug("workspace ws error", { tabId, workspaceId });
      }
    };
    ws.onclose = (e) => {
      if (mountedRef.current && wsSeqRef.current === seq) {
        setWsStatus("disconnected");
        wsLog("warn", "workspace close", {
          tabId,
          workspaceId,
          code: e.code,
          reason: e.reason,
          wasClean: e.wasClean,
        });
        terminalDebug("workspace ws close", { tabId, workspaceId, code: e.code, reason: e.reason, wasClean: e.wasClean });
        if (e.code === 1006 && !didOpen) {
          term.writeln(`\x1b[90mConnection to workspace "${workspaceName}" failed.\x1b[0m`);
          diagnoseApiReachability(API_BASE).then((hint) => {
            if (!mountedRef.current || wsSeqRef.current !== seq || !hint) return;
            term.writeln(`\x1b[90m${hint}\x1b[0m`);
          });
          if (retryCountRef.current < 1) {
            retryCountRef.current += 1;
            setTimeout(() => {
              if (!mountedRef.current || wsSeqRef.current !== seq) return;
              connectWebSocket(term, fit, true);
            }, 300);
          }
        } else if (e.code !== 1000 && e.code !== 1001 && didOpen) {
          term.writeln("\x1b[90mDisconnected.\x1b[0m");
        }
      }
    };

    return ws;
  }, [workspaceId, workspaceName]);

  useEffect(() => {
    mountedRef.current = true;

    if (!isActive) return;

    const container = termElRef.current;
    if (!container) return;

    if (!terminalInitializedRef.current) {
      terminalInitializedRef.current = true;

      const term = new XTerm({
        cursorBlink: true,
        theme: {
          background: "#0a0a0c",
          foreground: "#e0e0e0",
          cursor: "#e0e0e0",
          cursorAccent: "#0a0a0c",
          selectionBackground: "#3d4556",
          black: "#0d0d0d",
          brightBlack: "#4a4a4a",
          red: "#ff5555",
          brightRed: "#ff6e6e",
          green: "#50fa7b",
          brightGreen: "#69ff94",
          yellow: "#f1fa8c",
          brightYellow: "#ffffa5",
          blue: "#6272a4",
          brightBlue: "#8be9fd",
          magenta: "#bd93f9",
          brightMagenta: "#d6acff",
          cyan: "#8be9fd",
          brightCyan: "#a4ffff",
          white: "#bfbfbf",
          brightWhite: "#ffffff",
        },
        fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: 14,
        scrollback: 10000,
      });
      termRef.current = term;

      const fit = new FitAddon();
      fitRef.current = fit;
      term.loadAddon(fit);

      // Forward terminal input to WebSocket
      const onDataDisposable = term.onData((data) => {
        const ws = wsRef.current;
        if (ws?.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ t: "i", d: data }));
        }
      });

      // Resize handler
      const onResize = () => {
        if (!mountedRef.current) return;
        try {
          fit.fit();
          const ws = wsRef.current;
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ t: "r", c: term.cols, r: term.rows }));
          }
        } catch { /* ignore */ }
      };
      window.addEventListener("resize", onResize);

      // Defer opening to next frame to ensure container has dimensions
      let cancelled = false;
      rafOpenRef.current = requestAnimationFrame(() => {
        if (cancelled || !mountedRef.current) return;
        if (!mountedRef.current) return;
        try {
          term.open(container);
          rafFitRef.current = requestAnimationFrame(() => {
            if (cancelled || !mountedRef.current) return;
            try {
              fit.fit();
              terminalDebug("workspace terminal fit", { tabId, workspaceId, cols: term.cols, rows: term.rows });
            } catch { /* Ignore fit errors */ }
            term.writeln(`\x1b[90mConnecting to workspace: ${workspaceName}...\x1b[0m`);
            // Connect WebSocket after terminal is ready
            connectWebSocket(term, fit, false);
          });
        } catch (err) {
          terminalDebug("workspace terminal open failed", { tabId, workspaceId, error: String(err) });
        }
      });

      return () => {
        mountedRef.current = false;
        cancelled = true;
        if (rafOpenRef.current !== null) {
          cancelAnimationFrame(rafOpenRef.current);
          rafOpenRef.current = null;
        }
        if (rafFitRef.current !== null) {
          cancelAnimationFrame(rafFitRef.current);
          rafFitRef.current = null;
        }
        wsSeqRef.current += 1;
        window.removeEventListener("resize", onResize);
        try { onDataDisposable.dispose(); } catch { /* ignore */ }
        const ws = wsRef.current;
        if (ws) {
          try {
            ws.onopen = null;
            ws.onmessage = null;
            ws.onerror = null;
            ws.onclose = null;
          } catch { /* ignore */ }
          try { ws.close(); } catch { /* ignore */ }
        }
        try { term.dispose(); } catch { /* ignore */ }
        wsRef.current = null;
        termRef.current = null;
        fitRef.current = null;
        terminalInitializedRef.current = false;
      };
    }
  }, [isActive, connectWebSocket, workspaceName]);

  const reconnect = useCallback(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit || !mountedRef.current) return;
    connectWebSocket(term, fit, true);
  }, [connectWebSocket]);

  const reset = useCallback(() => {
    const ws = wsRef.current;
    if (ws?.readyState === WebSocket.OPEN) {
      try {
        ws.send(JSON.stringify({ t: "i", d: "reset\n" }));
        setTimeout(() => {
          try {
            ws.send(JSON.stringify({ t: "i", d: "stty sane\n" }));
          } catch { /* ignore */ }
        }, 50);
      } catch { /* ignore */ }
    } else {
      reconnect();
    }
  }, [reconnect]);

  useEffect(() => {
    if (isActive && fitRef.current) {
      const timer = setTimeout(() => {
        try { fitRef.current?.fit(); } catch { /* ignore */ }
      }, 50);
      return () => clearTimeout(timer);
    }
  }, [isActive]);

  useEffect(() => {
    if (isActive && onStatusChange) {
      onStatusChange(wsStatus, reconnect, reset);
    }
  }, [wsStatus, reconnect, reset, isActive, onStatusChange]);

  return (
    <div
      className={[
        "absolute inset-0 h-full min-h-0",
        isActive ? "opacity-100" : "pointer-events-none opacity-0",
      ].join(" ")}
      aria-label={`workspace-shell-tab-${tabId}`}
      ref={termElRef}
    />
  );
}

// Files Tab Component - Clean file explorer with drag-drop support
function FilesTab({ isActive }: { tabId: string; isActive: boolean }) {
  const [cwd, setCwd] = useState("/root/context");
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [fsLoading, setFsLoading] = useState(false);
  const [fsError, setFsError] = useState<string | null>(null);
  const [selected, setSelected] = useState<FsEntry | null>(null);
  const [uploading, setUploading] = useState<{
    done: number;
    total: number;
  } | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [isEditingPath, setIsEditingPath] = useState(false);
  const [editPath, setEditPath] = useState(cwd);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const pathInputRef = useRef<HTMLInputElement | null>(null);
  // Track the last loaded directory to avoid unnecessary reloads
  const lastLoadedDirRef = useRef<string | null>(null);
  const hasEverLoadedRef = useRef(false);
  const dragCounterRef = useRef(0);

  // Check if a file can be previewed
  const canPreview = useCallback((entry: FsEntry) => {
    return entry.kind === "file" && (isTextFile(entry.path) || isImageFile(entry.path));
  }, []);

  // Parse path into breadcrumb segments
  const breadcrumbs = useMemo(() => {
    const parts = cwd.split("/").filter(Boolean);
    const crumbs: { name: string; path: string }[] = [{ name: "/", path: "/" }];
    let accumulated = "";
    for (const part of parts) {
      accumulated += "/" + part;
      crumbs.push({ name: part, path: accumulated });
    }
    return crumbs;
  }, [cwd]);

  // Handle path edit submission
  const handlePathSubmit = useCallback(() => {
    const normalizedPath = editPath.trim() || "/";
    setIsEditingPath(false);
    if (normalizedPath !== cwd) {
      setCwd(normalizedPath);
    }
  }, [editPath, cwd]);

  // Start editing path
  const startEditingPath = useCallback(() => {
    setEditPath(cwd);
    setIsEditingPath(true);
    // Focus the input after render
    setTimeout(() => pathInputRef.current?.select(), 0);
  }, [cwd]);

  const sortedEntries = useMemo(() => {
    const dirs = entries
      .filter((e) => e.kind === "dir")
      .sort((a, b) => a.name.localeCompare(b.name));
    const files = entries
      .filter((e) => e.kind !== "dir")
      .sort((a, b) => a.name.localeCompare(b.name));
    return [...dirs, ...files];
  }, [entries]);

  const refreshDir = useCallback(async (path: string, force = false) => {
    // Skip if we already loaded this directory (unless forced)
    if (!force && lastLoadedDirRef.current === path && hasEverLoadedRef.current) {
      return;
    }
    
    setFsLoading(true);
    setFsError(null);
    try {
      const data = await listDir(path);
      setEntries(data);
      setSelected(null);
      lastLoadedDirRef.current = path;
      hasEverLoadedRef.current = true;
    } catch (e) {
      setFsError(e instanceof Error ? e.message : String(e));
    } finally {
      setFsLoading(false);
    }
  }, []);

  const handleUpload = useCallback(async (files: File[]) => {
    if (files.length === 0) return;
    setUploading({ done: 0, total: files.length });
    try {
      await uploadFiles(cwd, files, (done, total) =>
        setUploading({ done, total })
      );
      toast.success(`Uploaded ${files.length} file${files.length > 1 ? 's' : ''}`);
      await refreshDir(cwd, true);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setFsError(message);
      toast.error(`Upload failed: ${message}`);
    } finally {
      setUploading(null);
    }
  }, [cwd, refreshDir]);

  // Load directory when cwd changes or when becoming active for the first time
  useEffect(() => {
    if (isActive) {
      // Only reload if directory changed or never loaded
      void refreshDir(cwd, false);
    }
  }, [cwd, isActive, refreshDir]);
  
  // Force reload when cwd changes (user navigated)
  useEffect(() => {
    if (isActive && lastLoadedDirRef.current !== cwd) {
      void refreshDir(cwd, true);
    }
  }, [cwd, isActive, refreshDir]);

  // Drag and drop handlers for the entire file list
  const handleDragEnter = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounterRef.current++;
    if (e.dataTransfer.types.includes("Files")) {
      setIsDragging(true);
    }
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounterRef.current--;
    if (dragCounterRef.current === 0) {
      setIsDragging(false);
    }
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
  }, []);

  const handleDrop = useCallback(async (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounterRef.current = 0;
    setIsDragging(false);
    const files = Array.from(e.dataTransfer.files || []);
    await handleUpload(files);
  }, [handleUpload]);

  return (
    <div
      className={[
        "absolute inset-0 flex h-full min-h-0 flex-col p-4",
        isActive ? "opacity-100" : "pointer-events-none opacity-0",
      ].join(" ")}
    >
      {/* Compact toolbar */}
      <div className="mb-2 flex items-center gap-1.5">
        {/* Navigation buttons */}
        <button
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)] disabled:opacity-40 transition-colors"
          onClick={() => {
            const parts = cwd.split("/").filter(Boolean);
            if (parts.length === 0) return;
            parts.pop();
            setCwd("/" + parts.join("/"));
          }}
          disabled={cwd === "/"}
          title="Go up"
        >
          <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 15l7-7 7 7" />
          </svg>
        </button>

        <button
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)] transition-colors"
          onClick={() => void refreshDir(cwd, true)}
          title="Refresh"
        >
          <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
          </svg>
        </button>

        <div className="mx-1 h-4 w-px bg-white/10" />

        {/* Quick nav buttons */}
        <button
          className={`flex h-7 items-center gap-1.5 rounded-md px-2 text-xs transition-colors ${
            cwd === "/root/context"
              ? "bg-indigo-500/15 text-indigo-300"
              : "text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)]"
          }`}
          onClick={() => setCwd("/root/context")}
          title="User input files"
        >
          <span>📥</span>
          <span>context</span>
        </button>
        <button
          className={`flex h-7 items-center gap-1.5 rounded-md px-2 text-xs transition-colors ${
            cwd.startsWith("/root/work")
              ? "bg-indigo-500/15 text-indigo-300"
              : "text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)]"
          }`}
          onClick={() => setCwd("/root/work")}
          title="Agent workspace"
        >
          <span>🔨</span>
          <span>work</span>
        </button>
        <button
          className={`flex h-7 items-center gap-1.5 rounded-md px-2 text-xs transition-colors ${
            cwd.startsWith("/root/tools")
              ? "bg-indigo-500/15 text-indigo-300"
              : "text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)]"
          }`}
          onClick={() => setCwd("/root/tools")}
          title="Reusable tools"
        >
          <span>🛠️</span>
          <span>tools</span>
        </button>

        <div className="flex-1" />

        {/* Action buttons */}
        <button
          className="flex h-7 items-center gap-1.5 rounded-md px-2 text-xs text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)] transition-colors"
          onClick={async () => {
            const name = prompt("New folder name");
            if (!name) return;
            const target = cwd.endsWith("/") ? `${cwd}${name}` : `${cwd}/${name}`;
            try {
              await mkdir(target);
              toast.success(`Created folder ${name}`);
              await refreshDir(cwd, true);
            } catch (err) {
              toast.error(`Failed to create folder: ${err instanceof Error ? err.message : String(err)}`);
            }
          }}
          title="New folder"
        >
          <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 13h6m-3-3v6m-9 1V7a2 2 0 012-2h6l2 2h6a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
          </svg>
          <span>Folder</span>
        </button>

        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            const files = Array.from(e.target.files || []);
            void handleUpload(files);
            e.target.value = "";
          }}
        />
        <button
          className="flex h-7 items-center gap-1.5 rounded-md bg-indigo-500/15 px-2 text-xs text-indigo-300 hover:bg-indigo-500/25 transition-colors"
          onClick={() => fileInputRef.current?.click()}
          title="Upload files"
        >
          <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-8l-4-4m0 0L8 8m4-4v12" />
          </svg>
          <span>Import</span>
        </button>
      </div>

      {/* Breadcrumb navigation / Editable path */}
      <div className="mb-2 flex items-center text-xs group">
        <CopyButton text={cwd} label="Copied path" className="mr-1.5 opacity-60 group-hover:opacity-100" showOnHover={false} />
        {isEditingPath ? (
          <input
            ref={pathInputRef}
            type="text"
            value={editPath}
            onChange={(e) => setEditPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                handlePathSubmit();
              } else if (e.key === "Escape") {
                setIsEditingPath(false);
                setEditPath(cwd);
              }
            }}
            onBlur={handlePathSubmit}
            className="flex-1 rounded-md border border-indigo-500/30 bg-[var(--background)]/60 px-2 py-1 text-sm text-[var(--foreground)] focus:outline-none focus:border-indigo-500/50"
            autoFocus
          />
        ) : (
          <button
            className="flex items-center gap-1 overflow-x-auto scrollbar-none rounded-md px-1 py-0.5 hover:bg-white/[0.03] transition-colors group"
            onClick={startEditingPath}
            title="Click to edit path"
          >
            {breadcrumbs.map((crumb, i) => (
              <span key={crumb.path} className="flex items-center">
                {i > 0 && <span className="mx-0.5 text-[var(--foreground-muted)]">/</span>}
                <span
                  className={`rounded px-1 py-0.5 transition-colors ${
                    i === breadcrumbs.length - 1
                      ? "text-[var(--foreground)]"
                      : "text-[var(--foreground-muted)]"
                  }`}
                  onClick={(e) => {
                    e.stopPropagation();
                    setCwd(crumb.path);
                  }}
                >
                  {crumb.name}
                </span>
              </span>
            ))}
            <svg className="h-3 w-3 ml-1 text-[var(--foreground-muted)] opacity-0 group-hover:opacity-100 transition-opacity" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15.232 5.232l3.536 3.536m-2.036-5.036a2.5 2.5 0 113.536 3.536L6.5 21.036H3v-3.572L16.732 3.732z" />
            </svg>
          </button>
        )}
      </div>

      {/* Upload progress */}
      {uploading && (
        <div className="mb-2 flex items-center gap-2 rounded-md border border-indigo-500/30 bg-indigo-500/10 px-3 py-2 text-xs text-indigo-300">
          <svg className="h-3.5 w-3.5 animate-spin" fill="none" viewBox="0 0 24 24">
            <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
            <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
          </svg>
          <span>Uploading {uploading.done}/{uploading.total}...</span>
        </div>
      )}

      {fsError && (
        <div className="mb-2 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200 flex items-center gap-2">
          <svg className="h-3.5 w-3.5 flex-shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
          </svg>
          <span className="truncate">{fsError}</span>
          <button onClick={() => setFsError(null)} className="ml-auto text-red-300 hover:text-red-100">×</button>
        </div>
      )}

      {/* Main content area with drag-drop */}
      <div
        className={`flex-1 min-h-0 rounded-lg border transition-colors relative ${
          isDragging
            ? "border-indigo-500 bg-indigo-500/5"
            : "border-white/10 bg-black/20"
        }`}
        onDragEnter={handleDragEnter}
        onDragLeave={handleDragLeave}
        onDragOver={handleDragOver}
        onDrop={handleDrop}
      >
        {/* Drag overlay */}
        {isDragging && (
          <div className="absolute inset-0 z-10 flex items-center justify-center rounded-md bg-indigo-500/10 backdrop-blur-sm pointer-events-none">
            <div className="flex flex-col items-center gap-2 text-indigo-300">
              <svg className="h-8 w-8" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12" />
              </svg>
              <span className="text-sm font-medium">Drop files to upload</span>
            </div>
          </div>
        )}

        <div className="h-full flex flex-col">
          {/* Header */}
          <div className="grid grid-cols-12 gap-2 border-b border-white/5 px-3 py-1.5 text-[10px] uppercase tracking-wider text-[var(--foreground-muted)]">
            <div className="col-span-7">Name</div>
            <div className="col-span-3 text-right">Size</div>
            <div className="col-span-2 text-right">Type</div>
          </div>

          {/* File list */}
          <div className="flex-1 overflow-auto">
            {fsLoading ? (
              <div className="flex items-center justify-center py-8 text-sm text-[var(--foreground-muted)]">
                <svg className="h-4 w-4 animate-spin mr-2" fill="none" viewBox="0 0 24 24">
                  <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                  <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
                </svg>
                Loading...
              </div>
            ) : sortedEntries.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-12 text-[var(--foreground-muted)]">
                <svg className="h-8 w-8 mb-2 opacity-40" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
                </svg>
                <span className="text-sm">Empty folder</span>
                <span className="text-xs mt-1 opacity-60">Drag files here or click Import</span>
              </div>
            ) : (
              sortedEntries.map((e) => (
                <button
                  key={e.path}
                  className={`grid w-full grid-cols-12 gap-2 px-3 py-1.5 text-left text-sm transition-colors ${
                    selected?.path === e.path
                      ? "bg-indigo-500/10 text-[var(--foreground)]"
                      : "text-[var(--foreground)] hover:bg-white/[0.03]"
                  }`}
                  onClick={() => setSelected(e)}
                  onDoubleClick={() => {
                    if (e.kind === "dir") setCwd(e.path);
                    else if (canPreview(e)) setPreviewPath(e.path);
                    else void downloadFile(e.path);
                  }}
                >
                  <div className="col-span-7 flex items-center gap-2 truncate">
                    <span className="text-base flex-shrink-0">{e.kind === "dir" ? "📁" : canPreview(e) ? "👁️" : "📄"}</span>
                    <span className="truncate">{e.name}</span>
                  </div>
                  <div className="col-span-3 text-right text-[var(--foreground-muted)] tabular-nums">
                    {e.kind === "file" ? formatBytes(e.size) : "—"}
                  </div>
                  <div className="col-span-2 text-right text-[var(--foreground-muted)]">
                    {e.kind}
                  </div>
                </button>
              ))
            )}
          </div>

          {/* Footer with selection info */}
          {selected && (
            <div className="border-t border-white/5 px-3 py-2 flex items-center gap-3 text-xs bg-white/[0.02]">
              <span className="text-[var(--foreground-muted)] truncate flex-1">
                {selected.path}
              </span>
              {selected.kind === "file" && (
                <span className="text-[var(--foreground-muted)] tabular-nums">
                  {formatBytes(selected.size)}
                </span>
              )}
              <div className="flex items-center gap-1">
                {selected.kind === "file" && canPreview(selected) && (
                  <button
                    className="flex h-6 items-center gap-1 rounded bg-indigo-500/15 px-2 text-indigo-300 hover:bg-indigo-500/25 transition-colors"
                    onClick={() => setPreviewPath(selected.path)}
                    title="Preview"
                  >
                    <svg className="h-3 w-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M2.458 12C3.732 7.943 7.523 5 12 5c4.478 0 8.268 2.943 9.542 7-1.274 4.057-5.064 7-9.542 7-4.477 0-8.268-2.943-9.542-7z" />
                    </svg>
                  </button>
                )}
                {selected.kind === "file" && (
                  <button
                    className="flex h-6 items-center gap-1 rounded px-2 text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)] transition-colors"
                    onClick={() => void downloadFile(selected.path)}
                    title="Download"
                  >
                    <svg className="h-3 w-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
                    </svg>
                  </button>
                )}
                {selected.kind === "dir" && (
                  <button
                    className="flex h-6 items-center gap-1 rounded px-2 text-[var(--foreground-muted)] hover:bg-white/[0.05] hover:text-[var(--foreground)] transition-colors"
                    onClick={() => setCwd(selected.path)}
                    title="Open folder"
                  >
                    <svg className="h-3 w-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
                    </svg>
                  </button>
                )}
                <button
                  className="flex h-6 items-center gap-1 rounded bg-red-500/15 px-2 text-red-300 hover:bg-red-500/25 transition-colors"
                  onClick={async () => {
                    if (!confirm(`Delete ${selected.name}?`)) return;
                    try {
                      await rm(selected.path, selected.kind === "dir");
                      toast.success(`Deleted ${selected.name}`);
                      await refreshDir(cwd, true);
                    } catch (err) {
                      toast.error(`Failed to delete: ${err instanceof Error ? err.message : String(err)}`);
                    }
                  }}
                  title="Delete"
                >
                  <svg className="h-3 w-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                  </svg>
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* File Preview Modal */}
      {previewPath && (
        <FilePreviewModal
          path={previewPath}
          onClose={() => setPreviewPath(null)}
        />
      )}
    </div>
  );
}

const CONSOLE_TABS_KEY = 'console-tabs';

function loadSavedTabs(): { tabs: Tab[]; activeTabId: string } | null {
  if (typeof window === 'undefined') return null;
  try {
    const saved = localStorage.getItem(CONSOLE_TABS_KEY);
    if (saved) {
      const data = JSON.parse(saved);
      if (data.tabs && data.tabs.length > 0 && data.activeTabId) {
        return data;
      }
    }
  } catch {
    // ignore parse errors
  }
  return null;
}

function saveTabs(tabs: Tab[], activeTabId: string) {
  if (typeof window === 'undefined') return;
  try {
    localStorage.setItem(CONSOLE_TABS_KEY, JSON.stringify({ tabs, activeTabId }));
  } catch {
    // ignore storage errors
  }
}

// Compute initial tabs state once to avoid multiple loadSavedTabs() calls
// and fix the dependency issue where activeTabId initializer referenced tabs
function getInitialTabsState(): { tabs: Tab[]; activeTabId: string } {
  const saved = loadSavedTabs();
  if (saved) {
    return { tabs: saved.tabs, activeTabId: saved.activeTabId };
  }
  const defaultTabs: Tab[] = [
    { id: generateTabId(), type: "terminal", title: "Terminal 1" },
    { id: generateTabId(), type: "files", title: "Files 1" },
  ];
  return { tabs: defaultTabs, activeTabId: defaultTabs[0].id };
}

export default function ConsoleClient() {
  const searchParams = useSearchParams();
  const router = useRouter();

  // Initialize tabs and activeTabId from a single source to avoid race conditions
  const [{ tabs: initialTabs, activeTabId: initialActiveTabId }] = useState(getInitialTabsState);
  const [tabs, setTabs] = useState<Tab[]>(initialTabs);
  const [activeTabId, setActiveTabId] = useState<string>(initialActiveTabId);
  const [showNewTabMenu, setShowNewTabMenu] = useState(false);

  // Track if we've already processed URL params to avoid duplicate tab creation
  const processedWorkspaceRef = useRef<string | null>(null);

  // Terminal status tracking (for the active terminal tab)
  const [terminalStatus, setTerminalStatus] = useState<{
    status: "disconnected" | "connecting" | "connected" | "error";
    reconnect: () => void;
    reset: () => void;
  } | null>(null);

  const activeTab = tabs.find(t => t.id === activeTabId);
  const isTerminalActive = activeTab?.type === "terminal" || activeTab?.type === "workspace-shell";

  const handleTerminalStatusChange = useCallback((
    status: "disconnected" | "connecting" | "connected" | "error",
    reconnect: () => void,
    reset: () => void
  ) => {
    setTerminalStatus({ status, reconnect, reset });
  }, []);

  // Handle workspace URL parameter - create a workspace shell tab
  useEffect(() => {
    const workspaceId = searchParams.get('workspace');
    const workspaceName = searchParams.get('name');

    if (workspaceId && workspaceName && processedWorkspaceRef.current !== workspaceId) {
      processedWorkspaceRef.current = workspaceId;

      // Check if we already have a tab for this workspace
      const existingTab = tabs.find(
        t => t.type === 'workspace-shell' && t.workspaceId === workspaceId
      );

      if (existingTab) {
        // Just activate the existing tab
        setActiveTabId(existingTab.id);
      } else {
        // Create a new workspace shell tab
        const newTabId = generateTabId();
        const newTab: Tab = {
          id: newTabId,
          type: 'workspace-shell',
          title: workspaceName,
          workspaceId,
          workspaceName,
        };
        setTabs(prev => [...prev, newTab]);
        setActiveTabId(newTabId);
      }

      // Clear the URL params after processing
      router.replace('/console', { scroll: false });
    }
  }, [searchParams, tabs, router]);

  // Save tabs to localStorage whenever they change
  useEffect(() => {
    saveTabs(tabs, activeTabId);
  }, [tabs, activeTabId]);

  const addTab = (type: TabType) => {
    const newTabId = generateTabId();
    setTabs((prev) => {
      const terminalCount = prev.filter((t) => t.type === "terminal").length;
      const filesCount = prev.filter((t) => t.type === "files").length;
      const count = type === "terminal" ? terminalCount + 1 : filesCount + 1;
      const title = type === "terminal" ? `Terminal ${count}` : `Files ${count}`;
      return [...prev, { id: newTabId, type, title }];
    });
    setActiveTabId(newTabId);
    setShowNewTabMenu(false);
  };

  const closeTab = (tabId: string) => {
    setTabs((prev) => {
      if (prev.length <= 1) return prev;
      const idx = prev.findIndex((t) => t.id === tabId);
      const next = prev.filter((t) => t.id !== tabId);
      if (activeTabId === tabId) {
        const newIdx = Math.min(idx, next.length - 1);
        setActiveTabId(next[newIdx].id);
      }
      return next;
    });
  };

  return (
    <div className="flex min-h-screen flex-col p-4">
      {/* Main panel with integrated tab bar */}
      <div className="relative flex-1 min-h-0 flex flex-col rounded-lg border border-white/10 bg-[#0d0d0d] overflow-hidden">
        {/* Tab bar - inside the panel */}
        <div className="flex items-center border-b border-white/5 bg-white/[0.02]">
          <div className="flex items-center gap-1 flex-1">
            {tabs.map((tab) => (
              <div
                key={tab.id}
                className={`group flex items-center gap-2 px-3 py-2 text-sm cursor-pointer border-b-2 -mb-px transition-colors ${
                  activeTabId === tab.id
                    ? "border-indigo-500/70 text-[var(--foreground)] bg-white/[0.03]"
                    : "border-transparent text-[var(--foreground-muted)] hover:text-[var(--foreground)] hover:bg-white/[0.02]"
                }`}
                onClick={() => setActiveTabId(tab.id)}
              >
                <span className="text-sm opacity-70">
                  {tab.type === "terminal" ? "⌨️" : tab.type === "workspace-shell" ? "🖥️" : "📁"}
                </span>
                <span>{tab.title}</span>
                {tabs.length > 1 && (
                  <button
                    className="ml-1 opacity-0 group-hover:opacity-100 hover:bg-white/10 rounded p-0.5 transition-opacity"
                    onClick={(e) => {
                      e.stopPropagation();
                      closeTab(tab.id);
                    }}
                  >
                    <svg
                      className="w-3 h-3"
                      fill="none"
                      stroke="currentColor"
                      viewBox="0 0 24 24"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M6 18L18 6M6 6l12 12"
                      />
                    </svg>
                  </button>
                )}
              </div>
            ))}

            {/* Add tab button */}
            <div className="relative">
              <button
                className="flex items-center justify-center w-7 h-7 text-[var(--foreground-muted)] hover:text-[var(--foreground)] hover:bg-white/[0.05] rounded transition-colors"
                onClick={() => setShowNewTabMenu(!showNewTabMenu)}
              >
                <svg
                  className="w-3.5 h-3.5"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M12 4v16m8-8H4"
                  />
                </svg>
              </button>

              {showNewTabMenu && (
                <>
                  <div
                    className="fixed inset-0 z-10"
                    onClick={() => setShowNewTabMenu(false)}
                  />
                  <div className="absolute left-0 top-full mt-1 z-20 rounded-md border border-white/10 bg-[#1a1a1a] shadow-lg py-1 min-w-[140px]">
                    <button
                      className="w-full px-3 py-2 text-left text-sm text-[var(--foreground)] hover:bg-white/[0.05] flex items-center gap-2"
                      onClick={() => addTab("terminal")}
                    >
                      <span>⌨️</span> New Terminal
                    </button>
                    <button
                      className="w-full px-3 py-2 text-left text-sm text-[var(--foreground)] hover:bg-white/[0.05] flex items-center gap-2"
                      onClick={() => addTab("files")}
                    >
                      <span>📁</span> New Files
                    </button>
                  </div>
                </>
              )}
            </div>
          </div>

          {/* Status indicator (for terminal tabs) */}
          {isTerminalActive && terminalStatus && (
            <div className="flex items-center gap-3 px-3">
              <div className="flex items-center gap-2">
                <span
                  className={
                    terminalStatus.status === "connected"
                      ? "h-2 w-2 rounded-full bg-emerald-500"
                      : terminalStatus.status === "connecting"
                      ? "h-2 w-2 rounded-full bg-yellow-500 animate-pulse"
                      : terminalStatus.status === "error"
                      ? "h-2 w-2 rounded-full bg-red-500"
                      : "h-2 w-2 rounded-full bg-gray-500"
                  }
                />
                <span className="text-xs text-[var(--foreground-muted)]">
                  {terminalStatus.status}
                </span>
              </div>
              <button
                className="rounded px-2 py-1 text-xs text-[var(--foreground-muted)] hover:text-[var(--foreground)] hover:bg-white/[0.05] transition-colors"
                onClick={terminalStatus.reset}
              >
                Reset
              </button>
            </div>
          )}
        </div>

        {/* Tab content */}
        <div className="relative flex-1 min-h-0">
          {tabs.map((tab) =>
            tab.type === "terminal" ? (
              <TerminalTab
                key={tab.id}
                tabId={tab.id}
                isActive={activeTabId === tab.id}
                onStatusChange={handleTerminalStatusChange}
              />
            ) : tab.type === "workspace-shell" && tab.workspaceId && tab.workspaceName ? (
              <WorkspaceShellTab
                key={tab.id}
                tabId={tab.id}
                isActive={activeTabId === tab.id}
                workspaceId={tab.workspaceId}
                workspaceName={tab.workspaceName}
                onStatusChange={handleTerminalStatusChange}
              />
            ) : (
              <FilesTab
                key={tab.id}
                tabId={tab.id}
                isActive={activeTabId === tab.id}
              />
            )
          )}
        </div>
      </div>
    </div>
  );
}
