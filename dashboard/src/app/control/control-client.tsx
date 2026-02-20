"use client";

import { useEffect, useLayoutEffect, useMemo, useRef, useState, useCallback, memo, startTransition } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { toast } from "@/components/toast";
import { MarkdownContent } from "@/components/markdown-content";
import { StreamingMarkdown } from "@/components/streaming-markdown";
import {
  EnhancedInput,
  type SubmitPayload,
  type EnhancedInputHandle,
  type FilePasteContext,
} from "@/components/enhanced-input";
import { MissionAutomationsDialog } from "@/components/mission-automations-dialog";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { LazyJsonHighlighter } from "@/components/lazy-json-highlighter";
import { cn } from "@/lib/utils";
import { getMissionShortName } from "@/lib/mission-display";
import { getRuntimeApiBase } from "@/lib/settings";
import { authHeader } from "@/lib/auth";
import {
  cancelControl,
  postControlMessage,
  postControlToolResult,
  streamControl,
  loadMission,
  getMission,
  getMissionEvents,
  createMission,
  listMissions,
  setMissionStatus,
  resumeMission,
  getCurrentMission,
  uploadFile,
  uploadFileChunked,
  downloadFromUrl,
  formatBytes,
  getProgress,
  getRunningMissions,
  isNetworkError,
  cancelMission,
  listWorkspaces,
  getHealth,
  listDesktopSessions,
  closeDesktopSession,
  keepAliveDesktopSession,
  cleanupOrphanedDesktopSessions,
  cleanupStoppedDesktopSessions,
  removeFromQueue,
  clearQueue,
  getQueue,
  type StreamDiagnosticUpdate,
  type ControlRunState,
  type Mission,
  type MissionStatus,
  type RunningMissionInfo,
  type UploadProgress,
  type Workspace,
  type DesktopSessionDetail,
  type DesktopSessionStatus,
  type StoredEvent,
} from "@/lib/api";
import { QueueStrip, type QueueItem } from "@/components/queue-strip";
import {
  Send,
  Square,
  Bot,
  User,
  Loader,
  CheckCircle,
  XCircle,
  Ban,
  Clock,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  Target,
  Brain,
  Copy,
  Check,
  Paperclip,
  ArrowDown,
  Cpu,
  Layers,
  RefreshCw,
  RotateCcw,
  PlayCircle,
  Link2,
  ListPlus,
  X,
  Wrench,
  Terminal,
  FileText,
  Eye,
  Search,
  Globe,
  Code,
  FolderOpen,
  Trash2,
  Monitor,
  HelpCircle,
  PanelRightClose,
  PanelRight,
  Wifi,
  WifiOff,
  AlertTriangle,
  Download,
  Image,
  FileArchive,
  File,
  ExternalLink,
  MessageSquare,
} from "lucide-react";
import { IMAGE_PATH_PATTERN } from "@/lib/file-extensions";
import { insertTextAtSelection, type TextSelection } from "@/lib/text-selection";

type StreamDiagnosticsState = {
  phase: "idle" | "connecting" | "open" | "streaming" | "closed" | "error";
  url: string | null;
  status?: number;
  contentType?: string | null;
  cacheControl?: string | null;
  transferEncoding?: string | null;
  contentEncoding?: string | null;
  server?: string | null;
  via?: string | null;
  lastEventAt?: number;
  lastChunkAt?: number;
  bytes: number;
  lastError?: string | null;
};

type InputInsertionState = TextSelection & {
  insertedCount: number;
};

function formatDiagAge(ts?: number) {
  if (!ts) return "—";
  const deltaMs = Date.now() - ts;
  if (deltaMs < 0) return "—";
  const secs = Math.floor(deltaMs / 1000);
  if (secs < 5) return "just now";
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  const rem = secs % 60;
  if (mins < 60) return `${mins}m ${rem}s ago`;
  const hrs = Math.floor(mins / 60);
  const remMins = mins % 60;
  return `${hrs}h ${remMins}m ago`;
}

type StreamLogLevel = "debug" | "info" | "warn" | "error";

function streamLog(level: StreamLogLevel, message: string, meta?: Record<string, unknown>) {
  const prefix = "[control:sse]";
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
import {
  OptionList,
  OptionListErrorBoundary,
  parseSerializableOptionList,
  type OptionListSelection,
} from "@/components/tool-ui/option-list";
import {
  DataTable,
  parseSerializableDataTable,
} from "@/components/tool-ui/data-table";
import { useScrollToBottom } from "@/hooks/use-scroll-to-bottom";
import { useLocalStorage } from "@/hooks/use-local-storage";
import { useCopyToClipboard } from "@/hooks/use-copy-to-clipboard";
import { DesktopStream } from "@/components/desktop-stream";
import { NewMissionDialog } from "@/components/new-mission-dialog";
import { MissionSwitcher } from "@/components/mission-switcher";

import type { SharedFile } from "@/lib/api";

type CostSource = "actual" | "estimated" | "unknown";

type ChatItem =
  | {
      kind: "user";
      id: string;
      content: string;
      timestamp: number;
      queued?: boolean;
    }
  | {
      kind: "assistant";
      id: string;
      content: string;
      success: boolean;
      costCents: number;
      costSource: CostSource;
      model: string | null;
      timestamp: number;
      sharedFiles?: SharedFile[];
      resumable?: boolean;
    }
  | {
      kind: "thinking";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      // Streaming text delta (draft assistant output).
      kind: "stream";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      kind: "tool";
      id: string;
      toolCallId: string;
      name: string;
      args: unknown;
      result?: unknown;
      isUiTool: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      kind: "system";
      id: string;
      content: string;
      timestamp: number;
      resumable?: boolean;
      missionId?: string;
    }
  | {
      kind: "phase";
      id: string;
      phase: string;
      detail: string | null;
      agent: string | null;
    };

type ToolItem = Extract<ChatItem, { kind: "tool" }>;
type SidePanelItem = Extract<ChatItem, { kind: "thinking" | "stream" }>;

type QuestionOption = {
  label: string;
  description?: string;
};

type QuestionInfo = {
  header?: string;
  question?: string;
  options?: QuestionOption[];
  multiple?: boolean;
};

function parseQuestionArgs(args: unknown): QuestionInfo[] {
  if (!isRecord(args)) return [];
  const raw = args["questions"];
  if (!Array.isArray(raw)) return [];
  return raw
    .map((entry) => (isRecord(entry) ? entry : null))
    .filter((entry): entry is Record<string, unknown> => Boolean(entry))
    .map((entry) => ({
      header: typeof entry["header"] === "string" ? entry["header"] : undefined,
      question: typeof entry["question"] === "string" ? entry["question"] : undefined,
      options: Array.isArray(entry["options"])
        ? entry["options"]
            .map((opt) => (isRecord(opt) ? opt : null))
            .filter((opt): opt is Record<string, unknown> => Boolean(opt))
            .map((opt) => ({
              label: String(opt["label"] ?? ""),
              description:
                typeof opt["description"] === "string" ? opt["description"] : undefined,
            }))
            .filter((opt) => opt.label.length > 0)
        : [],
      multiple: Boolean(entry["multiple"] ?? entry["multiSelect"]),
    }))
    .filter((q) => (q.question?.length ?? 0) > 0);
}

function QuestionToolItem({
  item,
  onSubmit,
}: {
  item: ToolItem;
  onSubmit: (toolCallId: string, answers: string[][]) => Promise<void>;
}) {
  const questions = useMemo(() => parseQuestionArgs(item.args), [item.args]);
  const [answers, setAnswers] = useState<string[][]>(
    () => questions.map(() => [])
  );
  const [otherText, setOtherText] = useState<Record<number, string>>({});
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    setAnswers(questions.map(() => []));
    setOtherText({});
  }, [item.toolCallId, questions.length]);

  const hasResult = item.result !== undefined;

  const canSubmit = useMemo(() => {
    if (questions.length === 0) return false;
    return questions.every((_, idx) => (answers[idx] ?? []).length > 0);
  }, [answers, questions]);

  const handleToggle = (idx: number, label: string, multiple: boolean) => {
    setAnswers((prev) => {
      const next = [...prev];
      const current = new Set(next[idx] ?? []);
      if (multiple) {
        if (current.has(label)) {
          current.delete(label);
        } else {
          current.add(label);
        }
      } else {
        current.clear();
        current.add(label);
      }
      next[idx] = Array.from(current);
      return next;
    });
  };

  const handleSubmit = async () => {
    if (!canSubmit || submitting || hasResult) return;
    setSubmitting(true);
    try {
      const payload = questions.map((q, idx) => {
        const selections = answers[idx] ?? [];
        if (!selections.length) return [];
        const otherLabel = q.options?.find((opt) =>
          opt.label.toLowerCase().includes("other")
        )?.label;
        return selections.map((label) => {
          if (otherLabel && label === otherLabel) {
            const extra = otherText[idx]?.trim();
            return extra ? `Other: ${extra}` : label;
          }
          return label;
        });
      });
      await onSubmit(item.toolCallId, payload);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex justify-start gap-3">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
        <Bot className="h-4 w-4 text-indigo-400" />
      </div>
      <div className="max-w-[90%] rounded-2xl rounded-tl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
        <div className="mb-2 text-xs text-white/40">
          Tool: <span className="font-mono text-indigo-400">question</span>
        </div>
        {questions.length === 0 ? (
          <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-400">
            Failed to render question payload
          </div>
        ) : (
          <div className="space-y-4">
            {questions.map((q, idx) => {
              const multiple = Boolean(q.multiple);
              const selections = new Set(answers[idx] ?? []);
              return (
                <div key={`${item.toolCallId}-q-${idx}`} className="space-y-2">
                  <div className="text-sm font-medium text-white/90">
                    {q.header ? `${q.header}: ` : ""}
                    {q.question}
                  </div>
                  <div className="space-y-2">
                    {(q.options ?? []).map((opt) => {
                      const checked = selections.has(opt.label);
                      return (
                        <label
                          key={`${item.toolCallId}-q-${idx}-${opt.label}`}
                          className={cn(
                            "flex items-start gap-2 rounded-lg border px-3 py-2 text-sm transition-colors cursor-pointer",
                            checked
                              ? "border-indigo-500/40 bg-indigo-500/10"
                              : "border-white/10 hover:border-white/20"
                          )}
                        >
                          <input
                            type={multiple ? "checkbox" : "radio"}
                            checked={checked}
                            disabled={hasResult || submitting}
                            onChange={() => handleToggle(idx, opt.label, multiple)}
                            className="mt-0.5"
                          />
                          <div>
                            <div className="text-white/90">{opt.label}</div>
                            {opt.description && (
                              <div className="text-xs text-white/50">
                                {opt.description}
                              </div>
                            )}
                          </div>
                        </label>
                      );
                    })}
                  </div>
                  {(q.options ?? []).some((opt) =>
                    opt.label.toLowerCase().includes("other")
                  ) &&
                    selections.has(
                      (q.options ?? []).find((opt) =>
                        opt.label.toLowerCase().includes("other")
                      )?.label ?? ""
                    ) && (
                        <input
                          type="text"
                          value={otherText[idx] ?? ""}
                          onChange={(e) =>
                            setOtherText((prev) => ({
                              ...prev,
                              [idx]: e.target.value,
                            }))
                          }
                          placeholder="Add details…"
                          disabled={hasResult || submitting}
                          className="w-full rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2 text-sm text-white/80 focus:border-indigo-500/40 focus:outline-none"
                        />
                    )}
                </div>
              );
            })}
            {hasResult ? (
              <div className="text-xs text-green-400">Answer sent.</div>
            ) : (
              <button
                onClick={handleSubmit}
                disabled={!canSubmit || submitting}
                className={cn(
                  "inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium transition-colors",
                  !canSubmit || submitting
                    ? "bg-white/5 text-white/30 cursor-not-allowed"
                    : "bg-indigo-500/20 text-indigo-200 hover:bg-indigo-500/30"
                )}
              >
                {submitting ? "Sending…" : "Submit Answer"}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseCostSource(raw: unknown): CostSource {
  if (raw === "actual" || raw === "estimated" || raw === "unknown") {
    return raw;
  }
  return "unknown";
}

function parseCostAmount(raw: unknown): number | undefined {
  if (typeof raw === "number" && Number.isFinite(raw)) {
    return raw;
  }
  if (typeof raw === "string") {
    const parsed = Number(raw);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }
  return undefined;
}

function parseCostMetadata(
  meta: Record<string, unknown>,
  fallback?: { costCents: number; costSource: CostSource }
): { costCents: number; costSource: CostSource } {
  const cost = meta["cost"];
  if (isRecord(cost)) {
    const parsedAmount = parseCostAmount(cost["amount_cents"]);
    const hasSource = cost["source"] !== undefined;
    return {
      costCents: parsedAmount ?? fallback?.costCents ?? 0,
      costSource: hasSource ? parseCostSource(cost["source"]) : fallback?.costSource ?? "unknown",
    };
  }

  const parsedAmount = parseCostAmount(meta["cost_cents"]);
  const hasSource = meta["cost_source"] !== undefined;
  if (parsedAmount !== undefined || hasSource) {
    return {
      costCents: parsedAmount ?? fallback?.costCents ?? 0,
      costSource: hasSource ? parseCostSource(meta["cost_source"]) : fallback?.costSource ?? "unknown",
    };
  }

  return {
    costCents: fallback?.costCents ?? 0,
    costSource: fallback?.costSource ?? "unknown",
  };
}

/**
 * Generate a unique fingerprint for comparing message content.
 * Uses a delimiter that's unlikely to appear in message content to avoid
 * false matches when content contains newlines or role prefixes.
 */
function getMessageFingerprint(kind: string, content: string): string {
  return `${kind}\x00${content.length}\x00${content}`;
}

/**
 * Compare current items with mission history by content fingerprints.
 * Returns true if the history has changed (contents differ).
 */
function hasHistoryChanged(
  items: ReadonlyArray<{ kind: string; content?: string }>,
  history: ReadonlyArray<{ role: string; content?: string | null }>
): boolean {
  const currentFingerprints = items
    .filter(i => i.kind === "user" || i.kind === "assistant")
    .map(i => getMessageFingerprint(i.kind, i.content || ""));

  const newFingerprints = history.map(e =>
    getMessageFingerprint(e.role === "user" ? "user" : "assistant", e.content || "")
  );

  // If current items have MORE messages than API history, the API is stale (SSE delivered
  // messages that haven't been persisted yet). Don't replace - we'd lose messages.
  // But first verify the overlapping content matches to detect content mismatches.
  if (currentFingerprints.length > newFingerprints.length) {
    // Verify that all API messages match the corresponding local messages
    const hasContentMismatch = newFingerprints.some((fp, i) => fp !== currentFingerprints[i]);
    // If content matches, keep local (has more messages). If mismatch, history changed.
    return hasContentMismatch;
  }

  // If API has more messages, history has changed (e.g., messages from another session)
  if (currentFingerprints.length < newFingerprints.length) return true;

  // Same count - check if content differs
  return currentFingerprints.some((fp, i) => fp !== newFingerprints[i]);
}

function formatTime(timestamp: number): string {
  const date = new Date(timestamp);
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function statusLabel(state: ControlRunState): {
  label: string;
  Icon: typeof Loader;
  className: string;
} {
  switch (state) {
    case "idle":
      return { label: "Idle", Icon: Clock, className: "text-white/40" };
    case "running":
      return { label: "Running", Icon: Loader, className: "text-indigo-400" };
    case "waiting_for_tool":
      return { label: "Waiting", Icon: Loader, className: "text-amber-400" };
  }
}

function missionStatusLabel(status: MissionStatus): {
  label: string;
  className: string;
} {
  switch (status) {
    case "active":
      return { label: "Active", className: "bg-indigo-500/20 text-indigo-400" };
    case "completed":
      return {
        label: "Completed",
        className: "bg-emerald-500/20 text-emerald-400",
      };
    case "failed":
      return { label: "Failed", className: "bg-red-500/20 text-red-400" };
    case "interrupted":
      return { label: "Interrupted", className: "bg-amber-500/20 text-amber-400" };
    case "blocked":
      return { label: "Blocked", className: "bg-orange-500/20 text-orange-400" };
    case "not_feasible":
      return { label: "Not Feasible", className: "bg-rose-500/20 text-rose-400" };
  }
}

function missionStatusDotClass(status: MissionStatus): string {
  switch (status) {
    case "active":
      return "bg-emerald-400";
    case "completed":
      return "bg-emerald-400";
    case "failed":
      return "bg-red-400";
    case "interrupted":
      return "bg-amber-400";
    case "blocked":
      return "bg-orange-400";
    case "not_feasible":
      return "bg-red-400";
    default:
      return "bg-white/40";
  }
}

// Copy button component
function CopyButton({ text, className }: { text: string; className?: string }) {
  const [, copy] = useCopyToClipboard();
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    const success = await copy(text);
    if (success) {
      setCopied(true);
      toast.success("Copied to clipboard");
      setTimeout(() => setCopied(false), 2000);
    } else {
      toast.error("Failed to copy");
    }
  };

  return (
    <button
      onClick={handleCopy}
      className={cn(
        "p-1.5 rounded-lg transition-all",
        "opacity-0 group-hover:opacity-100",
        "hover:bg-white/[0.08] text-white/40 hover:text-white/70",
        className
      )}
      title="Copy message"
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-emerald-400" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </button>
  );
}

// Shimmer loading effect
function Shimmer({ className }: { className?: string }) {
  return (
    <div className={cn("animate-pulse", className)}>
      <div className="h-4 bg-white/[0.06] rounded w-3/4 mb-2" />
      <div className="h-4 bg-white/[0.06] rounded w-1/2 mb-2" />
      <div className="h-4 bg-white/[0.06] rounded w-5/6" />
    </div>
  );
}

function isTextPreviewableSharedFile(file: SharedFile): boolean {
  const name = (file.name || "").toLowerCase();
  if (file.content_type.startsWith("text/")) return true;
  if (file.content_type.includes("json") || file.content_type.includes("yaml") || file.content_type.includes("xml")) {
    return true;
  }
  return (
    name.endsWith(".txt") ||
    name.endsWith(".md") ||
    name.endsWith(".markdown") ||
    name.endsWith(".log") ||
    name.endsWith(".json") ||
    name.endsWith(".yaml") ||
    name.endsWith(".yml") ||
    name.endsWith(".toml") ||
    name.endsWith(".xml") ||
    name.endsWith(".csv") ||
    name.endsWith(".tsv")
  );
}

function getLanguageFromSharedFile(file: SharedFile): string {
  const name = (file.name || "").toLowerCase();
  if (name.endsWith(".md") || name.endsWith(".markdown") || file.content_type.includes("markdown")) return "markdown";
  if (name.endsWith(".json") || file.content_type.includes("json")) return "json";
  if (name.endsWith(".yaml") || name.endsWith(".yml") || file.content_type.includes("yaml")) return "yaml";
  if (name.endsWith(".xml") || file.content_type.includes("xml")) return "xml";
  if (name.endsWith(".csv")) return "csv";
  if (name.endsWith(".tsv")) return "tsv";
  return "text";
}

function SharedFilePreviewModal({
  file,
  resolvedUrl,
  isApiUrl,
  onClose,
  onDownload,
}: {
  file: SharedFile;
  resolvedUrl: string;
  isApiUrl: boolean;
  onClose: () => void;
  onDownload: () => void;
}) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState<string>("");
  const [copied, setCopied] = useState(false);
  const [sizeBytes, setSizeBytes] = useState<number | null>(null);

  const language = useMemo(() => getLanguageFromSharedFile(file), [file]);
  const isMarkdown = language === "markdown";

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      setLoading(true);
      setError(null);
      setText("");
      setSizeBytes(null);
      try {
        const res = await fetch(resolvedUrl, {
          headers: isApiUrl ? { ...authHeader() } : undefined,
        });
        if (!res.ok) throw new Error(`Failed to load (${res.status})`);
        const blob = await res.blob();
        const raw = await blob.text();
        const limit = 500_000;
        const finalText =
          raw.length > limit ? `${raw.slice(0, limit)}\n\n... (file truncated, too large to preview)` : raw;
        if (!cancelled) {
          setSizeBytes(blob.size);
          setText(finalText);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [isApiUrl, resolvedUrl]);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Ignore.
    }
  }, [text]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm pointer-events-none" />
      <div
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "relative rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl w-full max-w-4xl",
          "animate-in fade-in zoom-in-95 duration-200"
        )}
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-white/[0.06]">
          <div className="min-w-0">
            <h3 className="text-sm font-semibold text-white truncate">{file.name}</h3>
            <p className="text-xs text-white/40 truncate">
              {file.content_type}
              {sizeBytes != null && <span className="ml-2">• {formatBytes(sizeBytes)}</span>}
            </p>
          </div>
          <div className="flex items-center gap-2 shrink-0 ml-3">
            {!loading && !error && text && (
              <button
                onClick={handleCopy}
                className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
                title={copied ? "Copied" : "Copy"}
              >
                {copied ? <Check className="h-4 w-4 text-emerald-400" /> : <Copy className="h-4 w-4" />}
              </button>
            )}
            <button
              onClick={onDownload}
              className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
              title="Download"
            >
              <Download className="h-4 w-4" />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
              title="Close"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <div className="max-h-[70vh] overflow-auto">
          {loading ? (
            <div className="p-5">
              <Shimmer />
            </div>
          ) : error ? (
            <div className="p-5 text-sm text-red-400">{error}</div>
          ) : isMarkdown ? (
            <div className="p-5">
              <MarkdownContent content={text} />
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
                    fontFamily:
                      'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
                  },
                }}
              >
                {text}
              </SyntaxHighlighter>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// Shared file card component - renders images inline and other files as download cards
function SharedFileCard({ file }: { file: SharedFile }) {
  const iconMap: Record<SharedFile["kind"], typeof File> = {
    image: Image,
    document: FileText,
    archive: FileArchive,
    code: Code,
    other: File,
  };
  const FileIcon = iconMap[file.kind] || File;

  // Format file size
  const sizeLabel = file.size_bytes ? formatBytes(file.size_bytes) : null;

  const apiBase = getRuntimeApiBase();
  const isApiRelativeUrl = file.url.startsWith("/");
  const isApiUrl = isApiRelativeUrl || file.url.startsWith(apiBase);
  const resolvedUrl = isApiRelativeUrl ? `${apiBase}${file.url}` : file.url;
  const canPreview = isTextPreviewableSharedFile(file);

  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);

  // If this is an API-protected image, fetch it with auth and render from an object URL.
  useEffect(() => {
    if (file.kind !== "image") return;
    if (!isApiUrl) return; // External URLs can be loaded directly by the browser.

    let cancelled = false;
    let localUrl: string | null = null;

    const run = async () => {
      setLoading(true);
      setError(null);
      try {
        const res = await fetch(resolvedUrl, { headers: { ...authHeader() } });
        if (!res.ok) throw new Error(`Failed to load image (${res.status})`);
        const blob = await res.blob();
        localUrl = URL.createObjectURL(blob);
        if (!cancelled) setBlobUrl(localUrl);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void run();
    return () => {
      cancelled = true;
      if (localUrl) URL.revokeObjectURL(localUrl);
    };
  }, [file.kind, isApiUrl, resolvedUrl]);

  const handleDownload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // If URL is external, let the browser handle it.
      if (!isApiUrl) {
        window.open(resolvedUrl, "_blank", "noopener,noreferrer");
        return;
      }

      const res = await fetch(resolvedUrl, { headers: { ...authHeader() } });
      if (!res.ok) throw new Error(`Download failed (${res.status})`);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      try {
        const a = document.createElement("a");
        a.href = url;
        a.download = file.name || "download";
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
      } finally {
        URL.revokeObjectURL(url);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [file.name, isApiUrl, resolvedUrl]);

  const handleOpen = useCallback(() => {
    if (file.kind === "image" && blobUrl) {
      window.open(blobUrl, "_blank", "noopener,noreferrer");
      return;
    }
    if (!isApiUrl) {
      window.open(resolvedUrl, "_blank", "noopener,noreferrer");
      return;
    }
    // For API URLs we can't open directly without headers; download instead.
    void handleDownload();
  }, [blobUrl, file.kind, handleDownload, isApiUrl, resolvedUrl]);

  if (file.kind === "image") {
    // Render images inline (supports auth-protected API URLs).
    return (
      <div className="mt-3 rounded-lg overflow-hidden border border-white/[0.06] bg-black/20">
        <button type="button" onClick={handleOpen} className="block w-full text-left">
          {loading && !blobUrl ? (
            <div className="h-[240px] w-full animate-pulse bg-white/[0.03]" />
          ) : (
            <img
              src={blobUrl || resolvedUrl}
              alt={file.name}
              className="max-w-full max-h-[400px] object-contain"
              loading="lazy"
            />
          )}
        </button>
        <div className="flex items-center gap-2 px-3 py-2 text-xs text-white/40 border-t border-white/[0.06]">
          <Image className="h-3 w-3" />
          <span className="truncate flex-1">{file.name}</span>
          {sizeLabel && <span>{sizeLabel}</span>}
          <button
            type="button"
            onClick={handleOpen}
            className="text-indigo-400 hover:text-indigo-300 flex items-center gap-1"
            title="Open"
            aria-label="Open"
          >
            <ExternalLink className="h-3 w-3" />
          </button>
          <button
            type="button"
            onClick={handleDownload}
            className="text-indigo-400 hover:text-indigo-300 flex items-center gap-1"
            title="Download"
            aria-label="Download"
            disabled={loading}
          >
            <Download className={cn("h-3 w-3", loading && "animate-pulse")} />
          </button>
        </div>
        {error && (
          <div className="px-3 pb-2 text-xs text-red-400">{error}</div>
        )}
      </div>
    );
  }

  // Render other files as cards (download always, preview for text/markdown)
  return (
    <>
      <div
        className={cn(
          "mt-3 flex items-center gap-3 px-4 py-3 rounded-lg border border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04] transition-colors group",
          canPreview && "cursor-pointer"
        )}
        onClick={() => {
          if (canPreview) setPreviewOpen(true);
        }}
        role={canPreview ? "button" : undefined}
        tabIndex={canPreview ? 0 : undefined}
        onKeyDown={(e) => {
          if (!canPreview) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setPreviewOpen(true);
          }
        }}
      >
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-indigo-500/10">
          <FileIcon className="h-5 w-5 text-indigo-400" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="font-medium text-sm text-white/80 truncate">{file.name}</div>
          <div className="text-xs text-white/40 flex items-center gap-2">
            <span className="truncate">{file.content_type}</span>
            {sizeLabel && (
              <>
                <span>•</span>
                <span>{sizeLabel}</span>
              </>
            )}
          </div>
          {error && <div className="mt-1 text-xs text-red-400">{error}</div>}
        </div>

        {canPreview && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setPreviewOpen(true);
            }}
            className="p-2 rounded-md text-white/30 group-hover:text-indigo-400 hover:bg-white/[0.06] transition-colors"
            title="Preview"
            aria-label="Preview"
            disabled={loading}
          >
            <Eye className={cn("h-4 w-4", loading && "animate-pulse")} />
          </button>
        )}

        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            void handleDownload();
          }}
          className="p-2 rounded-md text-white/30 group-hover:text-indigo-400 hover:bg-white/[0.06] transition-colors"
          title="Download"
          aria-label="Download"
          disabled={loading}
        >
          <Download className={cn("h-4 w-4", loading && "animate-pulse")} />
        </button>
      </div>

      {previewOpen && canPreview && (
        <SharedFilePreviewModal
          file={file}
          resolvedUrl={resolvedUrl}
          isApiUrl={isApiUrl}
          onClose={() => setPreviewOpen(false)}
          onDownload={() => void handleDownload()}
        />
      )}
    </>
  );
}

// Phase indicator - shows what the agent is doing during preparation
function PhaseItem({ item }: { item: Extract<ChatItem, { kind: "phase" }> }) {
  const phaseLabels: Record<string, { label: string; icon: typeof Brain }> = {
    estimating_complexity: { label: "Analyzing task", icon: Brain },
    selecting_model: { label: "Selecting model", icon: Cpu },
    splitting_task: { label: "Decomposing task", icon: Target },
    executing: { label: "Executing", icon: Loader },
    verifying: { label: "Verifying", icon: CheckCircle },
  };

  const { label, icon: Icon } = phaseLabels[item.phase] ?? {
    label: item.phase.replace(/_/g, " "),
    icon: Brain,
  };

  return (
    <div className="flex items-center gap-3 py-3 animate-fade-in">
      <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-indigo-500/10">
        <Icon className="h-4 w-4 text-indigo-400 animate-pulse" />
      </div>
      <div className="flex flex-col">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-indigo-400">{label}</span>
          {item.agent && (
            <span className="text-[10px] font-mono text-white/30 bg-white/[0.04] px-1.5 py-0.5 rounded">
              {item.agent}
            </span>
          )}
        </div>
        {item.detail && (
          <span className="text-xs text-white/40">{item.detail}</span>
        )}
      </div>
      <div className="ml-auto">
        <Loader className="h-3 w-3 text-indigo-400/50 animate-spin" />
      </div>
    </div>
  );
}

// Thinking group component - displays multiple thinking items merged with separators
function ThinkingGroupItem({
  items,
  basePath,
  workspaceId,
  missionId,
}: {
  items: SidePanelItem[];
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}) {
  // Filter out empty items for display
  const nonEmptyItems = useMemo(() =>
    items.filter(item => item.content.trim()),
    [items]
  );

  const hasActiveItem = items.some(item => !item.done);
  const [expanded, setExpanded] = useState(hasActiveItem);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const hasAutoCollapsedRef = useRef(false);

  // Get the earliest start time and latest end time
  const startTime = Math.min(...items.map(item => item.startTime));
  const endTime = items.every(item => item.done && item.endTime)
    ? Math.max(...items.map(item => item.endTime || item.startTime))
    : undefined;

  // Update elapsed time while any thinking is active
  useEffect(() => {
    if (!hasActiveItem) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [hasActiveItem, startTime]);

  // Auto-collapse when all thinking is done
  useEffect(() => {
    if (!hasActiveItem && expanded && !hasAutoCollapsedRef.current) {
      const duration = Math.floor((Date.now() - startTime) / 1000);
      if (duration > 30) {
        hasAutoCollapsedRef.current = true;
        return;
      }
      const timer = setTimeout(() => {
        setExpanded(false);
        hasAutoCollapsedRef.current = true;
      }, 1500);
      return () => clearTimeout(timer);
    }
  }, [hasActiveItem, expanded, startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds <= 0) return "<1s";
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  const duration = !hasActiveItem && endTime
    ? formatDuration(Math.floor((endTime - startTime) / 1000))
    : formatDuration(elapsedSeconds);

  // If no non-empty items, don't render anything
  if (nonEmptyItems.length === 0) {
    return null;
  }

  const label = (() => {
    const hasStream = nonEmptyItems.some((item) => item.kind === "stream");
    const hasThinking = nonEmptyItems.some((item) => item.kind === "thinking");
    if (hasStream && !hasThinking) {
      return nonEmptyItems.length === 1 ? "Draft" : "Drafts";
    }
    return nonEmptyItems.length === 1 ? "Thought" : "Thoughts";
  })();

  const activeLabel = (() => {
    if (items.some((item) => !item.done && item.kind === "thinking")) {
      return "Thinking";
    }
    if (items.some((item) => !item.done && item.kind === "stream")) {
      return "Streaming";
    }
    return "Thinking";
  })();

  return (
    <div className="my-2">
      {/* Compact header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className={cn(
          "flex items-center gap-1.5 px-2.5 py-1 rounded-full",
          "bg-white/[0.04] border border-white/[0.06]",
          "text-white/40 hover:text-white/60 hover:bg-white/[0.06]",
          "transition-all duration-200"
        )}
      >
        <Brain
          className={cn(
            "h-3 w-3",
            hasActiveItem && "animate-pulse text-indigo-400"
          )}
        />
        <span className="text-xs">
          {hasActiveItem
            ? `${activeLabel} for ${duration}`
            : `${label} for ${duration}`}
        </span>
        {nonEmptyItems.length > 1 && (
          <span className="text-xs text-white/30">({nonEmptyItems.length})</span>
        )}
        <ChevronDown
          className={cn(
            "h-3 w-3 transition-transform duration-200",
            expanded ? "rotate-0" : "-rotate-90"
          )}
        />
      </button>

      {/* Expandable content with animation */}
      <div
        className={cn(
          "overflow-hidden transition-all duration-200 ease-out",
          expanded ? "max-h-[50vh] opacity-100 mt-2" : "max-h-0 opacity-0"
        )}
      >
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
          <div className="overflow-y-auto max-h-[45vh] leading-relaxed space-y-2">
            {nonEmptyItems.map((item, idx) => (
              <div key={item.id}>
                {idx > 0 && (
                  <div className="border-t border-white/[0.06] my-2" />
                )}
                {/* Use StreamingMarkdown for efficient incremental rendering */}
                <StreamingMarkdown
                  content={item.content}
                  isStreaming={!item.done}
                  className="text-xs text-white/60 [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
                  basePath={basePath}
                  workspaceId={workspaceId}
                  missionId={missionId}
                />
              </div>
            ))}
            {hasActiveItem && nonEmptyItems.length === 0 && (
              <span className="italic text-white/30">Processing...</span>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// Thinking panel item - simplified version for side panel
// Threshold for collapsing long thoughts (in characters)
const THOUGHT_COLLAPSE_THRESHOLD = 800;

function ThinkingPanelItem({
  item,
  isActive,
  basePath,
  workspaceId,
  missionId,
}: {
  item: SidePanelItem;
  isActive: boolean;
  basePath?: string;
  workspaceId?: string;
  missionId?: string;
}) {
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const [isExpanded, setIsExpanded] = useState(false);

  useEffect(() => {
    if (item.done) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [item.done, item.startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds <= 0) return "<1s";
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  const duration = item.done && item.endTime
    ? formatDuration(Math.floor((item.endTime - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  const activeLabel = item.kind === "stream" ? "Streaming" : "Thinking";
  const pastLabel = item.kind === "stream" ? "Draft" : "Thought";

  // For completed items, check if content is long enough to collapse
  const isLongContent = !isActive && item.content.length > THOUGHT_COLLAPSE_THRESHOLD;
  const shouldTruncate = isLongContent && !isExpanded;
  
  // Get truncated content for display
  const displayContent = shouldTruncate
    ? item.content.slice(0, THOUGHT_COLLAPSE_THRESHOLD) + "..."
    : item.content;

  return (
    <div className={cn(
      "rounded-lg border p-3",
      // Unified styling - subtle border highlight for active, same base appearance
      isActive
        ? "border-indigo-500/30 bg-white/[0.02]"
        : "border-white/[0.06] bg-white/[0.02]"
    )}>
      <div className="flex items-center gap-2 mb-2">
        <Brain
          className={cn(
            "h-3.5 w-3.5 shrink-0",
            isActive ? "animate-pulse text-indigo-400" : "text-white/40"
          )}
        />
        <span className={cn(
          "text-xs font-medium",
          isActive ? "text-indigo-400" : "text-white/50"
        )}>
          {isActive
            ? `${activeLabel} for ${duration}`
            : `${pastLabel} for ${duration}`}
        </span>
      </div>
      {/* Content area - no internal scroll, unified text color */}
      <div className="text-xs leading-relaxed text-white/60">
        {item.content ? (
          <>
            <StreamingMarkdown
              content={displayContent}
              isStreaming={isActive}
              className="text-xs [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
              basePath={basePath}
              workspaceId={workspaceId}
              missionId={missionId}
            />
            {/* Expand/collapse button for long content */}
            {isLongContent && (
              <button
                onClick={() => setIsExpanded(!isExpanded)}
                className="mt-2 text-[10px] text-indigo-400/70 hover:text-indigo-400 transition-colors flex items-center gap-1"
              >
                {isExpanded ? (
                  <>
                    <ChevronUp className="h-3 w-3" />
                    Show less
                  </>
                ) : (
                  <>
                    <ChevronDown className="h-3 w-3" />
                    Show more ({Math.round((item.content.length - THOUGHT_COLLAPSE_THRESHOLD) / 100) * 100}+ chars)
                  </>
                )}
              </button>
            )}
          </>
        ) : (
          <span className="italic text-white/30">Processing...</span>
        )}
      </div>
    </div>
  );
}

// Thinking side panel component
function ThinkingPanel({
  items,
  onClose,
  className,
  basePath,
  missionId,
}: {
  items: SidePanelItem[];
  onClose: () => void;
  className?: string;
  basePath?: string;
  missionId?: string | null;
}) {
  const activeItems = useMemo(
    () => items.filter((t) => !t.done),
    [items]
  );
  const hasActiveThinking = activeItems.some((i) => i.kind === "thinking");
  const hasActiveStream = activeItems.some((i) => i.kind === "stream");

  // Deduplicate completed items by content - keep first occurrence
  const completedItems = useMemo(() => {
    const seen = new Set<string>();
    return items.filter(t => {
      if (!t.done) return false;
      // Skip empty/whitespace-only content
      const trimmed = t.content.trim();
      if (!trimmed) return false;
      if (seen.has(trimmed)) return false;
      seen.add(trimmed);
      return true;
    });
  }, [items]);

  // Performance: limit visible thoughts, load more on demand
  const INITIAL_VISIBLE_THOUGHTS = 10;
  const LOAD_MORE_THOUGHTS = 10;
  const [visibleThoughtsLimit, setVisibleThoughtsLimit] = useState(INITIAL_VISIBLE_THOUGHTS);

  // Reset limit when mission changes (not during streaming updates)
  useEffect(() => {
    setVisibleThoughtsLimit(INITIAL_VISIBLE_THOUGHTS);
  }, [missionId]);

  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when active thought content changes
  useEffect(() => {
    if (scrollRef.current && activeItems.length > 0) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [activeItems.map((i) => `${i.id}:${i.content.length}`).join("|")]);

  // Handle Escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  return (
    <div className={cn("w-full h-full flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden animate-slide-in-right", className)}>
      {/* Header */}
      <div className="flex items-center justify-between border-b border-white/[0.06] px-4 py-3">
        <div className="flex items-center gap-2">
          <Brain className={cn(
            "h-4 w-4",
            activeItems.length > 0 ? "animate-pulse text-indigo-400" : "text-white/40"
          )} />
          <span className="text-sm font-medium text-white">
            {hasActiveThinking ? "Thinking" : hasActiveStream ? "Streaming" : "Thoughts"}
          </span>
          {(completedItems.length > 0 || activeItems.length > 0) && (
            <span className="text-xs text-white/30">
              ({completedItems.length + activeItems.length})
            </span>
          )}
        </div>
        <button
          onClick={onClose}
          className="flex h-6 w-6 items-center justify-center rounded-lg text-white/40 hover:bg-white/[0.04] hover:text-white transition-colors"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {/* Content - flex-col with overflow, scrolls up for history */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto p-3 space-y-3 flex flex-col">
        {items.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-center p-4">
            <Brain className="h-8 w-8 text-white/20 mb-3" />
            <p className="text-sm text-white/40">No thoughts yet</p>
            <p className="text-xs text-white/30 mt-1">
              Agent reasoning will appear here
            </p>
          </div>
        ) : (
          <>
            {/* Spacer to push content to bottom when not enough to fill */}
            <div className="flex-1" />

            {/* Completed thoughts (history - scroll up to see) */}
            {completedItems.length > 0 && (
              <>
                {/* Load more button if there are hidden thoughts */}
                {completedItems.length > visibleThoughtsLimit && (
                  <button
                    onClick={() => setVisibleThoughtsLimit(prev => prev + LOAD_MORE_THOUGHTS)}
                    className="w-full py-1.5 px-3 text-[10px] text-white/40 hover:text-white/60 hover:bg-white/5 rounded-lg transition-colors flex items-center justify-center gap-1.5"
                  >
                    <ChevronUp className="w-3 h-3" />
                    Load {Math.min(LOAD_MORE_THOUGHTS, completedItems.length - visibleThoughtsLimit)} older
                    <span className="text-white/25">
                      ({completedItems.length - visibleThoughtsLimit} hidden)
                    </span>
                  </button>
                )}
                {completedItems.slice(-visibleThoughtsLimit).map((item) => (
                  <ThinkingPanelItem key={item.id} item={item} isActive={false} basePath={basePath} />
                ))}
                {activeItems.length > 0 && (
                  <div className="text-[10px] uppercase tracking-wider text-white/30 px-1">
                    Current
                  </div>
                )}
              </>
            )}

            {/* Active items at the bottom (sticky) */}
            {activeItems.map((item) => (
              <ThinkingPanelItem
                key={item.id}
                item={item}
                isActive={true}
                basePath={basePath}
              />
            ))}
          </>
        )}
      </div>
    </div>
  );
}

// Get icon for tool based on its name
function getToolIcon(toolName: string) {
  const name = toolName.toLowerCase();
  if (name.includes("bash") || name.includes("shell") || name.includes("terminal") || name.includes("exec")) {
    return Terminal;
  }
  if (name.includes("read") || name.includes("file") || name.includes("write")) {
    return FileText;
  }
  if (name.includes("search") || name.includes("grep") || name.includes("find")) {
    return Search;
  }
  if (name.includes("browser") || name.includes("web") || name.includes("http") || name.includes("url")) {
    return Globe;
  }
  if (name.includes("code") || name.includes("edit") || name.includes("patch")) {
    return Code;
  }
  if (name.includes("list") || name.includes("dir") || name.includes("ls")) {
    return FolderOpen;
  }
  return Wrench;
}

// Format tool arguments for display
function formatToolArgs(args: unknown): string {
  if (args === null || args === undefined) return "";
  if (typeof args === "string") return args;
  try {
    return JSON.stringify(args, null, 2);
  } catch {
    return String(args);
  }
}

// Truncate text for preview
function truncateText(text: string, maxLength: number = 100): string {
  if (text.length <= maxLength) return text;
  return text.slice(0, maxLength) + "...";
}

// Check if a tool is a subagent/background task tool
function isSubagentTool(toolName: string): boolean {
  const name = toolName.toLowerCase();
  return (
    name === "background_task" ||
    name === "task" ||
    name.includes("subagent") ||
    name.includes("spawn_agent") ||
    name.includes("delegate")
  );
}

// Extract subagent info from tool args
function extractSubagentInfo(args: unknown): {
  agentName: string | null;
  description: string | null;
  prompt: string | null;
} {
  if (!args || typeof args !== "object") {
    return { agentName: null, description: null, prompt: null };
  }
  const argsObj = args as Record<string, unknown>;
  return {
    agentName: typeof argsObj.agent === "string" ? argsObj.agent :
               typeof argsObj.subagent_type === "string" ? argsObj.subagent_type :
               typeof argsObj.name === "string" ? argsObj.name : null,
    description: typeof argsObj.description === "string" ? argsObj.description : null,
    prompt: typeof argsObj.prompt === "string" ? argsObj.prompt : null,
  };
}

// Parse subagent result for summary stats
function parseSubagentResult(result: unknown): {
  success: boolean;
  cancelled: boolean;
  summary: string | null;
} {
  if (!result) return { success: false, cancelled: false, summary: null };

  // Handle string results
  if (typeof result === "string") {
    // Strip out <task_metadata>...</task_metadata> blocks entirely
    const cleanedResult = result.replace(/<task_metadata>[\s\S]*?<\/task_metadata>/gi, "").trim();
    // Check for explicit error indicators at the start, not just keyword presence
    const trimmedLower = cleanedResult.toLowerCase();
    const isError = trimmedLower.startsWith("error:") ||
                    trimmedLower.startsWith("error -") ||
                    trimmedLower.startsWith("failed:") ||
                    trimmedLower.startsWith("exception:");
    // Try to extract a meaningful summary from the result
    const lines = cleanedResult.split("\n").filter(l => l.trim());
    const summary = lines.length > 0 ? truncateText(lines[0], 100) : null;
    return { success: !isError, cancelled: false, summary };
  }

  // Handle object results
  if (typeof result === "object") {
    const resultObj = result as Record<string, unknown>;
    const statusLower = typeof resultObj.status === "string" ? resultObj.status.toLowerCase() : "";
    const isCancelled = statusLower === "cancelled";
    const isError = !isCancelled && (
                    resultObj.error !== undefined ||
                    resultObj.is_error === true ||
                    resultObj.success === false ||
                    statusLower === "error" || statusLower === "failed");
    const summary = typeof resultObj.summary === "string" ? resultObj.summary :
                    typeof resultObj.message === "string" ? resultObj.message :
                    typeof resultObj.reason === "string" ? resultObj.reason :
                    typeof resultObj.result === "string" ? truncateText(resultObj.result, 100) : null;
    return { success: !isError && !isCancelled, cancelled: isCancelled, summary };
  }

  return { success: true, cancelled: false, summary: null };
}

// Subagent/Background Task tool item with enhanced UX
// Memoized to prevent re-renders when parent state changes
const SubagentToolItem = memo(function SubagentToolItem({
  item,
}: {
  item: Extract<ChatItem, { kind: "tool" }>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const isDone = item.result !== undefined;

  // Memoize subagent info extraction
  const { agentName, description, prompt } = useMemo(
    () => extractSubagentInfo(item.args),
    [item.args]
  );

  // Memoize result parsing
  const { success, cancelled, summary } = useMemo(
    () => (isDone ? parseSubagentResult(item.result) : { success: false, cancelled: false, summary: null }),
    [isDone, item.result]
  );

  // Update elapsed time while tool is running
  useEffect(() => {
    if (isDone) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [isDone, item.startTime]);

  const formatDuration = (seconds: number) => {
    // Handle negative or zero durations
    if (seconds <= 0) return "<1s";
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  const duration = isDone && item.endTime
    ? formatDuration(Math.floor((item.endTime - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  // Memoize result string formatting
  const resultStr = useMemo(
    () => (item.result !== undefined ? formatToolArgs(item.result) : null),
    [item.result]
  );

  return (
    <div className="my-3">
      {/* Main card */}
      <div
        className={cn(
          "rounded-lg border overflow-hidden",
          "bg-white/[0.02]",
          !isDone && "border-purple-500/30",
          isDone && cancelled && "border-amber-500/20",
          isDone && success && !cancelled && "border-emerald-500/20",
          isDone && !success && !cancelled && "border-red-500/20"
        )}
      >
        {/* Header */}
        <button
          onClick={() => setExpanded(!expanded)}
          className={cn(
            "w-full flex items-center gap-3 px-3 py-2",
            "hover:bg-white/[0.02] transition-colors"
          )}
        >
          {/* Icon */}
          <div className={cn(
            "flex-shrink-0 w-8 h-8 rounded-lg flex items-center justify-center",
            !isDone && "bg-purple-500/20",
            isDone && cancelled && "bg-amber-500/20",
            isDone && success && !cancelled && "bg-emerald-500/20",
            isDone && !success && !cancelled && "bg-red-500/20"
          )}>
            {!isDone ? (
              <Cpu className="h-4 w-4 text-purple-400 animate-pulse" />
            ) : cancelled ? (
              <XCircle className="h-4 w-4 text-amber-400" />
            ) : success ? (
              <CheckCircle className="h-4 w-4 text-emerald-400" />
            ) : (
              <XCircle className="h-4 w-4 text-red-400" />
            )}
          </div>

          {/* Info */}
          <div className="flex-1 text-left min-w-0">
            <div className="flex items-center gap-2">
              <span className={cn(
                "text-sm font-medium",
                !isDone && "text-purple-300",
                isDone && cancelled && "text-amber-300",
                isDone && success && !cancelled && "text-emerald-300",
                isDone && !success && !cancelled && "text-red-300"
              )}>
                {agentName || "Subagent"}
              </span>
              {description && (
                <span className="text-xs text-white/40 truncate">
                  {truncateText(description, 40)}
                </span>
              )}
            </div>

            {/* Status line */}
            <div className="flex items-center gap-2 mt-0.5">
              {!isDone ? (
                <>
                  <span className="text-xs text-white/50">Running for {duration}</span>
                  <Loader className="h-3 w-3 animate-spin text-purple-400" />
                </>
              ) : cancelled ? (
                <>
                  <span className="text-xs text-amber-400">Cancelled</span>
                  {summary && (
                    <span className="text-xs text-white/40 truncate max-w-[200px]">
                      — {summary}
                    </span>
                  )}
                </>
              ) : (
                <>
                  <span className="text-xs text-white/50">Completed in {duration}</span>
                  {summary && (
                    <span className="text-xs text-white/40 truncate max-w-[200px]">
                      — {summary}
                    </span>
                  )}
                </>
              )}
            </div>
          </div>

          {/* Peek toggle */}
          <div className="flex items-center gap-1 flex-shrink-0">
            <span className={cn(
              "text-[10px] uppercase tracking-wider transition-colors",
              expanded ? "text-white/50" : "text-white/30"
            )}>
              {expanded ? "Hide" : "Peek"}
            </span>
            <ChevronDown
              className={cn(
                "h-4 w-4 text-white/30 transition-transform duration-200",
                expanded ? "rotate-0" : "-rotate-90"
              )}
            />
          </div>
        </button>

        {/* Progress bar (only when running) */}
        {!isDone && (
          <div className="h-1 bg-purple-500/10">
            <div
              className="h-full bg-purple-500/50 animate-pulse"
              style={{
                width: "100%",
                background: "linear-gradient(90deg, transparent, rgba(168, 85, 247, 0.5), transparent)",
                animation: "shimmer 2s infinite"
              }}
            />
          </div>
        )}

        {/* Expandable content — conditionally rendered (issue #156) */}
        {expanded && (
        <div>
          <div className="px-3 py-3 space-y-3 border-t border-white/[0.06]">
            {/* Prompt preview */}
            {prompt && (
              <div>
                <div className="text-[10px] uppercase tracking-wider text-white/30 mb-1">
                  Task
                </div>
                <div className="text-xs text-white/60 bg-black/20 rounded p-2 max-h-24 overflow-y-auto">
                  {truncateText(prompt, 300)}
                </div>
              </div>
            )}

            {/* Result */}
            {resultStr !== null && (
              <div>
                <div className={cn(
                  "text-[10px] uppercase tracking-wider mb-1",
                  !success ? "text-red-400/70" : "text-emerald-400/70"
                )}>
                  {!success ? "Error" : "Result"}
                </div>
                <div className={cn(
                  "max-h-60 overflow-y-auto rounded",
                  !success && "[&_pre]:!bg-red-500/10"
                )}>
                  <LazyJsonHighlighter
                    background={!success ? "rgba(239, 68, 68, 0.1)" : undefined}
                    textColor={!success ? "rgb(248, 113, 113)" : undefined}
                  >
                    {resultStr}
                  </LazyJsonHighlighter>
                </div>
              </div>
            )}
          </div>
        </div>
        )}
      </div>
    </div>
  );
});

// Extract image file paths from tool result strings
// Matches patterns like "/path/to/image.png" or "screenshots/file.jpg"
function extractImagePaths(text: string): string[] {
  const paths: string[] = [];
  // Use shared pattern from file-extensions.ts
  // Reset regex state since it's global
  IMAGE_PATH_PATTERN.lastIndex = 0;
  const matches = text.match(IMAGE_PATH_PATTERN);
  if (matches) {
    for (const match of matches) {
      // Normalize and dedupe
      const normalized = match.trim();
      if (!paths.includes(normalized)) {
        paths.push(normalized);
      }
    }
  }
  return paths;
}

// Component to display an image preview with click-to-open functionality
function ImagePreview({
  path,
  workspaceId,
  missionId,
}: {
  path: string;
  workspaceId?: string;
  missionId?: string;
}) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const loadImage = async () => {
      setLoading(true);
      setError(null);
      try {
        const API_BASE = getRuntimeApiBase();
        const params = new URLSearchParams({ path });
        if (workspaceId) params.set("workspace_id", workspaceId);
        if (missionId) params.set("mission_id", missionId);
        const res = await fetch(`${API_BASE}/api/fs/download?${params.toString()}`, {
          headers: { ...authHeader() },
        });
        if (!res.ok) {
          throw new Error(`Failed to load image: ${res.status}`);
        }
        const blob = await res.blob();
        if (cancelled) return;
        const url = URL.createObjectURL(blob);
        setImageUrl(url);
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : 'Failed to load image');
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    loadImage();
    return () => {
      cancelled = true;
      if (imageUrl) URL.revokeObjectURL(imageUrl);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, workspaceId, missionId]);

  const openInNewTab = () => {
    if (imageUrl) {
      window.open(imageUrl, '_blank');
    }
  };

  const fileName = path.split('/').pop() || path;

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-xs text-white/40 py-2">
        <Loader className="h-3 w-3 animate-spin" />
        <span>Loading {fileName}...</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center gap-2 text-xs text-red-400/70 py-2">
        <AlertTriangle className="h-3 w-3" />
        <span>{error}</span>
      </div>
    );
  }

  return (
    <div className="mt-2">
      <div className="text-[10px] uppercase tracking-wider text-white/30 mb-1 flex items-center gap-2">
        <Image className="h-3 w-3" />
        Screenshot Preview
      </div>
      <div
        className="relative group cursor-pointer rounded-lg overflow-hidden border border-white/10 hover:border-white/20 transition-colors"
        onClick={openInNewTab}
        title="Click to open in new tab"
      >
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img
          src={imageUrl || ''}
          alt={fileName}
          className="max-w-full max-h-60 object-contain bg-black/20"
        />
        <div className="absolute inset-0 bg-black/0 group-hover:bg-black/30 transition-colors flex items-center justify-center opacity-0 group-hover:opacity-100">
          <div className="flex items-center gap-2 text-white text-sm bg-black/60 px-3 py-1.5 rounded-full">
            <ExternalLink className="h-4 w-4" />
            Open in new tab
          </div>
        </div>
      </div>
      <div className="text-[10px] text-white/30 mt-1 truncate">{path}</div>
    </div>
  );
}

// Tool call item component with collapsible UI
// Memoized to prevent re-renders when parent state changes
const ToolCallItem = memo(function ToolCallItem({
  item,
  workspaceId,
  missionId,
}: {
  item: Extract<ChatItem, { kind: "tool" }>;
  workspaceId?: string;
  missionId?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const isDone = item.result !== undefined;
  const ToolIcon = getToolIcon(item.name);

  // Update elapsed time while tool is running
  useEffect(() => {
    if (isDone) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [isDone, item.startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds <= 0) return "<1s";
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  // Use endTime for completed tools, otherwise use elapsed time for running tools
  const duration = isDone && item.endTime
    ? formatDuration(Math.floor((item.endTime - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  // Memoize expensive string formatting - only recompute when item.args changes
  const argsStr = useMemo(() => formatToolArgs(item.args), [item.args]);

  // Memoize result string - only recompute when item.result changes
  const resultStr = useMemo(
    () => (item.result !== undefined ? formatToolArgs(item.result) : null),
    [item.result]
  );

  // Memoize cancelled detection - check if tool was cancelled due to mission ending
  const isCancelled = useMemo(() => {
    if (typeof item.result === "object" && item.result !== null) {
      const resultObj = item.result as Record<string, unknown>;
      return resultObj.status === "cancelled";
    }
    return false;
  }, [item.result]);

  // Memoize error detection - only recompute when result changes
  const isError = useMemo(() => {
    if (resultStr === null || isCancelled) return false;

    // Check if the result is an object with explicit error fields
    if (typeof item.result === "object" && item.result !== null) {
      const resultObj = item.result as Record<string, unknown>;
      if (resultObj.error !== undefined || resultObj.is_error === true || resultObj.success === false) {
        return true;
      }
    }

    // Check if the string result starts with error indicators (more specific than keyword search)
    const trimmedLower = resultStr.trim().toLowerCase();
    return trimmedLower.startsWith("error:") ||
           trimmedLower.startsWith("error -") ||
           trimmedLower.startsWith("failed:") ||
           trimmedLower.startsWith("exception:");
  }, [item.result, resultStr, isCancelled]);

  // Memoize args preview - only recompute when item.args changes
  const argsPreview = useMemo(
    () => truncateText(
      typeof item.args === "object" && item.args !== null
        ? Object.keys(item.args as Record<string, unknown>).slice(0, 2).join(", ")
        : argsStr,
      50
    ),
    [item.args, argsStr]
  );

  return (
    <div className="my-2">
      {/* Compact header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className={cn(
          "flex items-center gap-1.5 px-2.5 py-1 rounded-full",
          "bg-white/[0.04] border border-white/[0.06]",
          "text-white/40 hover:text-white/60 hover:bg-white/[0.06]",
          "transition-all duration-200",
          !isDone && "border-amber-500/20",
          isDone && isCancelled && "border-amber-500/20",
          isDone && !isError && !isCancelled && "border-emerald-500/20",
          isDone && isError && "border-red-500/20"
        )}
      >
        <ToolIcon
          className={cn(
            "h-3 w-3",
            !isDone && "animate-pulse text-amber-400",
            isDone && isCancelled && "text-amber-400",
            isDone && !isError && !isCancelled && "text-emerald-400",
            isDone && isError && "text-red-400"
          )}
        />
        <span className="text-xs font-mono text-indigo-400">{item.name}</span>
        {argsPreview && (
          <span className="text-xs text-white/30 truncate max-w-[150px]">
            ({argsPreview})
          </span>
        )}
        <span className="text-xs text-white/30 ml-1">
          {isDone ? (isCancelled ? "cancelled" : duration) : `${duration}...`}
        </span>
        {isDone && !isError && !isCancelled && <CheckCircle className="h-3 w-3 text-emerald-400" />}
        {isDone && isCancelled && <XCircle className="h-3 w-3 text-amber-400" />}
        {isDone && isError && <XCircle className="h-3 w-3 text-red-400" />}
        {!isDone && <Loader className="h-3 w-3 animate-spin text-amber-400" />}
        <ChevronDown
          className={cn(
            "h-3 w-3 transition-transform duration-200 ml-1",
            expanded ? "rotate-0" : "-rotate-90"
          )}
        />
      </button>

      {/* Expandable content — conditionally rendered to avoid mounting
          SyntaxHighlighter while collapsed (issue #156). */}
      {expanded && (
      <div className="mt-2">
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3 space-y-3">
          {/* Arguments */}
          {argsStr && (
            <div>
              <div className="text-[10px] uppercase tracking-wider text-white/30 mb-1">
                Arguments
              </div>
              <div className="max-h-40 overflow-y-auto rounded">
                <LazyJsonHighlighter>
                  {argsStr}
                </LazyJsonHighlighter>
              </div>
            </div>
          )}

          {/* Result */}
          {resultStr !== null && (
            <div>
              <div className={cn(
                "text-[10px] uppercase tracking-wider mb-1",
                isError ? "text-red-400/70" : "text-emerald-400/70"
              )}>
                {isError ? "Error" : "Result"}
              </div>
              <div className={cn(
                "max-h-40 overflow-y-auto rounded",
                isError && "[&_pre]:!bg-red-500/10"
              )}>
                <LazyJsonHighlighter
                  background={isError ? "rgba(239, 68, 68, 0.1)" : undefined}
                  textColor={isError ? "rgb(248, 113, 113)" : undefined}
                >
                  {resultStr}
                </LazyJsonHighlighter>
              </div>
              {/* Image previews for screenshot results - only from tools that produce images */}
              {(() => {
                // Only extract images from tools that actually produce screenshots
                const IMAGE_PRODUCING_TOOLS = ['capture', 'screenshot', 'desktop_screenshot', 'mccli', 'browser_take_screenshot'];
                const toolName = item.name.toLowerCase();
                if (!IMAGE_PRODUCING_TOOLS.some(t => toolName.includes(t))) return null;

                const imagePaths = extractImagePaths(resultStr);
                if (imagePaths.length === 0) return null;
                return (
                  <div className="space-y-2">
                    {imagePaths.map((path) => (
                      <ImagePreview
                        key={path}
                        path={path}
                        workspaceId={workspaceId}
                        missionId={missionId}
                      />
                    ))}
                  </div>
                );
              })()}
            </div>
          )}

          {/* Still running indicator */}
          {!isDone && (
            <div className="flex items-center gap-2 text-xs text-amber-400/70">
              <Loader className="h-3 w-3 animate-spin" />
              <span>Running for {duration}...</span>
            </div>
          )}
        </div>
      </div>
      )}
    </div>
  );
});

// Collapsed tool group component - shows last tool with expand option
function CollapsedToolGroup({
  tools,
  isExpanded,
  onToggleExpand,
  workspaceId,
  missionId,
}: {
  tools: Extract<ChatItem, { kind: "tool" }>[];
  isExpanded: boolean;
  onToggleExpand: () => void;
  workspaceId?: string;
  missionId?: string;
}) {
  const hiddenCount = tools.length - 1;
  const lastTool = tools[tools.length - 1];

  // Helper to render appropriate tool component
  const renderTool = (tool: Extract<ChatItem, { kind: "tool" }>) => {
    if (isSubagentTool(tool.name)) {
      return <SubagentToolItem key={tool.id} item={tool} />;
    }
    return (
      <ToolCallItem
        key={tool.id}
        item={tool}
        workspaceId={workspaceId}
        missionId={missionId}
      />
    );
  };

  if (isExpanded) {
    // Show all tools with a collapse button at the top
    return (
      <div className="space-y-2">
        <button
          onClick={onToggleExpand}
          className={cn(
            "flex items-center gap-1.5 px-2.5 py-1 rounded-full",
            "bg-white/[0.02] border border-white/[0.04]",
            "text-white/30 hover:text-white/50 hover:bg-white/[0.04]",
            "transition-all duration-200 text-xs"
          )}
        >
          <ChevronUp className="h-3 w-3" />
          <span>Hide {hiddenCount} previous tool{hiddenCount > 1 ? "s" : ""}</span>
        </button>
        {tools.map((tool) => renderTool(tool))}
      </div>
    );
  }

  // Collapsed state - show expand button + last tool
  return (
    <div className="space-y-2">
      <button
        onClick={onToggleExpand}
        className={cn(
          "flex items-center gap-1.5 px-2.5 py-1 rounded-full",
          "bg-white/[0.02] border border-white/[0.04]",
          "text-white/30 hover:text-white/50 hover:bg-white/[0.04]",
          "transition-all duration-200 text-xs"
        )}
      >
        <ChevronDown className="h-3 w-3" />
        <span>Show {hiddenCount} previous tool{hiddenCount > 1 ? "s" : ""}</span>
      </button>
      {renderTool(lastTool)}
    </div>
  );
}

// Attachment preview component
function AttachmentPreview({
  file,
  isUploading,
  onRemove,
}: {
  file: { name: string; type: string };
  isUploading?: boolean;
  onRemove?: () => void;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.06]">
      <Paperclip className="h-4 w-4 text-white/40" />
      <span className="text-sm text-white/70 truncate max-w-[200px]">
        {file.name}
      </span>
      {isUploading ? (
        <Loader className="h-3 w-3 animate-spin text-indigo-400" />
      ) : (
        onRemove && (
          <button
            onClick={onRemove}
            className="text-white/40 hover:text-white/70 transition-colors"
          >
            <XCircle className="h-4 w-4" />
          </button>
        )
      )}
    </div>
  );
}

export default function ControlClient() {
  const searchParams = useSearchParams();
  const router = useRouter();

  const [items, setItems] = useState<ChatItem[]>([]);
  const itemsRef = useRef<ChatItem[]>([]);
  const [draftInput, setDraftInput] = useLocalStorage("control-draft", "");
  const [input, setInput] = useState(draftInput);
  const [canSubmitInput, setCanSubmitInput] = useState(false);
  const [lastMissionId, setLastMissionId] = useLocalStorage<string | null>(
    "control-last-mission-id",
    null
  );

  const [runState, setRunState] = useState<ControlRunState>("idle");
  const [runStateMissionId, setRunStateMissionId] = useState<string | null>(
    null
  );
  const [queueLen, setQueueLen] = useState(0);
  const lastQueueLenRef = useRef<number | null>(null);
  const syncingQueueRef = useRef(false);

  // Performance optimization: limit rendered items for large conversations
  const INITIAL_VISIBLE_ITEMS = 30;
  const LOAD_MORE_INCREMENT = 30;
  const [visibleItemsLimit, setVisibleItemsLimit] = useState(INITIAL_VISIBLE_ITEMS);

  // Connection state for SSE stream - starts as disconnected until first event received
  const [connectionState, setConnectionState] = useState<
    "connected" | "disconnected" | "reconnecting"
  >("disconnected");
  const [reconnectAttempt, setReconnectAttempt] = useState(0);
  const [showStreamDiagnostics, setShowStreamDiagnostics] = useState(false);
  const [streamDiagnostics, setStreamDiagnostics] = useState<StreamDiagnosticsState>({
    phase: "idle",
    url: null,
    bytes: 0,
    lastError: null,
  });
  const [diagTick, setDiagTick] = useState(0);

  // Progress state (for "Subtask X of Y" indicator), tracked per mission
  const [progressByMission, setProgressByMission] = useState<
    Record<
      string,
      {
        total: number;
        completed: number;
        current: string | null;
        depth: number;
      }
    >
  >({});

  // Mission state
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [viewingMission, setViewingMission] = useState<Mission | null>(null);
  const [missionLoading, setMissionLoading] = useState(false);
  const [recentMissions, setRecentMissions] = useState<Mission[]>([]);
  const [dismissedResumeUI, setDismissedResumeUI] = useState(false);

  // Workspaces for mission creation
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);

  // Library context for agents

  // Only tick when stream is active to avoid unnecessary re-renders
  const streamIsActive = streamDiagnostics.phase === "open" || streamDiagnostics.phase === "streaming" || streamDiagnostics.phase === "connecting";
  useEffect(() => {
    if (!streamIsActive) return;
    const interval = setInterval(() => setDiagTick((prev) => prev + 1), 1000);
    return () => clearInterval(interval);
  }, [streamIsActive]);

  // Parallel missions state
  const [runningMissions, setRunningMissions] = useState<RunningMissionInfo[]>(
    []
  );
  const [showMissionSwitcher, setShowMissionSwitcher] = useState(false);
  const [showAutomationsDialog, setShowAutomationsDialog] = useState(false);

  // Track which mission's events we're viewing (for parallel missions)
  // This can differ from currentMission when viewing a parallel mission
  const [viewingMissionId, setViewingMissionId] = useState<string | null>(null);

  // Store items per mission to preserve context when switching
  // Limited to MAX_CACHED_MISSIONS to prevent memory bloat
  const MAX_CACHED_MISSIONS = 5;
  const [missionItems, setMissionItems] = useState<Record<string, ChatItem[]>>(
    {}
  );

  // Helper to update missionItems with LRU-style cleanup
  const updateMissionItems = useCallback((missionId: string, items: ChatItem[]) => {
    setMissionItems((prev) => {
      const updated = { ...prev, [missionId]: items };
      const keys = Object.keys(updated);
      // If over limit, remove oldest entries (first in object)
      if (keys.length > MAX_CACHED_MISSIONS) {
        const toRemove = keys.slice(0, keys.length - MAX_CACHED_MISSIONS);
        toRemove.forEach(k => delete updated[k]);
      }
      return updated;
    });
  }, []);

  // Attachment state
  const [attachments, setAttachments] = useState<
    { file: File; uploading: boolean }[]
  >([]);
  const [uploadQueue, setUploadQueue] = useState<string[]>([]);
  const [uploadProgress, setUploadProgress] = useState<{
    fileName: string;
    progress: UploadProgress;
  } | null>(null);
  const [showUrlInput, setShowUrlInput] = useState(false);
  const [urlInput, setUrlInput] = useState("");
  const [urlDownloading, setUrlDownloading] = useState(false);

  // Server configuration (fetched from health endpoint)
  const [maxIterations, setMaxIterations] = useState<number>(50); // Default fallback

  // Desktop stream state
  const [showDesktopStream, setShowDesktopStream] = useState(false);
  const [desktopDisplayId, setDesktopDisplayId] = useState(":99");
  const desktopDisplayIdRef = useRef(":99");
  const [showDisplaySelector, setShowDisplaySelector] = useState(false);
  const [hasDesktopSession, setHasDesktopSession] = useState(false);
  const [desktopSessions, setDesktopSessions] = useState<DesktopSessionDetail[]>([]);
  const desktopSessionsRef = useRef<DesktopSessionDetail[]>([]);
  const hasDesktopSessionRef = useRef(false);
  const [isClosingDesktop, setIsClosingDesktop] = useState<string | null>(null);
  // Track when we're expecting a desktop session (from ToolCall before ToolResult arrives)
  const expectingDesktopSessionRef = useRef(false);
  const desktopRapidPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Thinking panel state
  const [showThinkingPanel, setShowThinkingPanel] = useState(false);
  const [thinkingPanelManuallyHidden, setThinkingPanelManuallyHidden] = useState(false);
  const handleToggleThinkingPanel = useCallback(() => {
    setShowThinkingPanel((prev) => {
      const next = !prev;
      setThinkingPanelManuallyHidden(!next);
      return next;
    });
  }, []);

  const handleCloseThinkingPanel = useCallback(() => {
    setShowThinkingPanel(false);
    setThinkingPanelManuallyHidden(true);
  }, []);

  const adjustVisibleItemsLimit = useCallback((historyItems: ChatItem[]) => {
    let lastAssistantIdx = -1;
    for (let i = historyItems.length - 1; i >= 0; i--) {
      if (historyItems[i].kind === "assistant") {
        lastAssistantIdx = i;
        break;
      }
    }

    if (lastAssistantIdx === -1) {
      setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
      return;
    }

    const required = historyItems.length - lastAssistantIdx;
    if (required <= INITIAL_VISIBLE_ITEMS) {
      setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
      return;
    }

    setVisibleItemsLimit(required);
  }, []);

  const HISTORY_EVENT_TYPES = useMemo(
    () => [
      "user_message",
      "assistant_message",
      "tool_call",
      "tool_result",
      "text_delta",
      "thinking",
    ],
    []
  );
  const loadHistoryEvents = useCallback(
    async (id: string) => {
      const PAGE_LIMIT = 1000;
      const MAX_EVENTS = 20000;
      const MAX_PAGES = 200;
      const all: StoredEvent[] = [];
      const seenIds = new Set<number>();
      let offset = 0;
      for (let page = 0; page < MAX_PAGES; page += 1) {
        const batch = await getMissionEvents(id, {
          types: HISTORY_EVENT_TYPES,
          limit: PAGE_LIMIT,
          offset,
        });
        if (!Array.isArray(batch) || batch.length === 0) break;

        let newCount = 0;
        for (const event of batch) {
          if (seenIds.has(event.id)) continue;
          seenIds.add(event.id);
          all.push(event);
          newCount += 1;
        }

        if (batch.length < PAGE_LIMIT) break;
        if (newCount === 0) break; // avoid infinite loops if offset is ignored server-side
        if (all.length >= MAX_EVENTS) break;
        offset += batch.length;
      }

      all.sort((a, b) => {
        if (a.sequence !== b.sequence) return a.sequence - b.sequence;
        const ta = new Date(a.timestamp).getTime();
        const tb = new Date(b.timestamp).getTime();
        if (ta !== tb) return ta - tb;
        return a.id - b.id;
      });

      return all;
    },
    [HISTORY_EVENT_TYPES]
  );

  // Tool groups expansion state - tracks which groups are expanded by their first tool's id
  const [expandedToolGroups, setExpandedToolGroups] = useState<Set<string>>(new Set());

  const dedupedItems = useMemo(() => {
    if (items.length <= 1) return items;
    const seen = new Set<string>();
    const out: ChatItem[] = [];
    for (let i = items.length - 1; i >= 0; i--) {
      const item = items[i];
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      out.push(item);
    }
    return out.reverse();
  }, [items]);

  const displayItems = useMemo(() => {
    if (!dedupedItems.some((item) => item.kind === "user" && item.queued)) {
      return dedupedItems;
    }
    const queued: ChatItem[] = [];
    const normal: ChatItem[] = [];
    for (const item of dedupedItems) {
      if (item.kind === "user" && item.queued) {
        queued.push(item);
      } else {
        normal.push(item);
      }
    }
    return [...normal, ...queued];
  }, [dedupedItems]);

  const lastNonQueuedItem = useMemo(() => {
    for (let i = displayItems.length - 1; i >= 0; i--) {
      const item = displayItems[i];
      if (!(item.kind === "user" && item.queued)) {
        return item;
      }
    }
    return displayItems[displayItems.length - 1];
  }, [displayItems]);

  // Queued messages should render only in the QueueStrip above the input.
  // When a queued message is dequeued (queued=false), it will appear in chat normally.
  const chatDisplayItems = useMemo(
    () => displayItems.filter((it) => !(it.kind === "user" && it.queued === true)),
    [displayItems]
  );

  // Extract thinking + streaming items for the side panel.
  const thinkingItems = useMemo(
    () =>
      dedupedItems.filter(
        (it): it is SidePanelItem =>
          it.kind === "thinking" || it.kind === "stream"
      ),
    [dedupedItems]
  );

  // Deduplicated count for display (same logic as ThinkingPanel)
  const thinkingItemsCount = useMemo(() => {
    const activeCount = thinkingItems.filter((t) => !t.done).length;
    const seen = new Set<string>();
    let completedCount = 0;
    for (const t of thinkingItems) {
      if (!t.done) continue;
      const trimmed = t.content.trim();
      if (!trimmed || seen.has(trimmed)) continue;
      seen.add(trimmed);
      completedCount++;
    }
    return completedCount + activeCount;
  }, [thinkingItems]);

  // Check if there's active thinking happening
  const hasActiveThinking = useMemo(() =>
    thinkingItems.some(t => !t.done),
    [thinkingItems]
  );

  // Auto-show thinking panel when thinking starts (only on transition to active)
  const prevHasActiveThinking = useRef(false);
  useEffect(() => {
    desktopSessionsRef.current = desktopSessions;
  }, [desktopSessions]);

  useEffect(() => {
    desktopDisplayIdRef.current = desktopDisplayId;
  }, [desktopDisplayId]);

  useEffect(() => {
    hasDesktopSessionRef.current = hasDesktopSession;
  }, [hasDesktopSession]);

  useEffect(() => {
    // Only auto-show when transitioning from no active thinking to active thinking
    if (hasActiveThinking && !prevHasActiveThinking.current && !thinkingPanelManuallyHidden) {
      setShowThinkingPanel(true);
    }
    prevHasActiveThinking.current = hasActiveThinking;
  }, [hasActiveThinking, thinkingPanelManuallyHidden]);

  useEffect(() => {
    setThinkingPanelManuallyHidden(false);
  }, [viewingMissionId]);

  // Group consecutive tool items and thinking items for collapsed display
  // Returns array of: original items OR { kind: "tool_group", tools: [...] } OR { kind: "thinking_group", thoughts: [...] }
  type ToolGroup = {
    kind: "tool_group";
    groupId: string;
    tools: Extract<ChatItem, { kind: "tool" }>[];
  };
  type ThinkingGroup = {
    kind: "thinking_group";
    groupId: string;
    thoughts: SidePanelItem[];
  };
  type GroupedItem = ChatItem | ToolGroup | ThinkingGroup;

  const groupedItems = useMemo((): GroupedItem[] => {
    const result: GroupedItem[] = [];
    let currentToolGroup: Extract<ChatItem, { kind: "tool" }>[] = [];
    let currentThinkingGroup: SidePanelItem[] = [];

    const flushToolGroup = () => {
      if (currentToolGroup.length === 0) return;
      if (currentToolGroup.length === 1) {
        result.push(currentToolGroup[0]);
      } else {
        result.push({
          kind: "tool_group",
          groupId: currentToolGroup[0].id,
          tools: currentToolGroup,
        });
      }
      currentToolGroup = [];
    };

    const flushThinkingGroup = () => {
      if (currentThinkingGroup.length === 0) return;
      // Always create a group for thinking items (even for single items)
      // This ensures consistent rendering through ThinkingGroupItem
      result.push({
        kind: "thinking_group",
        groupId: currentThinkingGroup[0].id,
        thoughts: currentThinkingGroup,
      });
      currentThinkingGroup = [];
    };

    for (const item of chatDisplayItems) {
      if (item.kind === "tool" && !item.isUiTool) {
        // Non-UI tool - flush thinking first, then add to tool group
        flushThinkingGroup();
        currentToolGroup.push(item);
      } else if (item.kind === "thinking" || item.kind === "stream") {
        if (showThinkingPanel) {
          // When thinking panel is open, skip all thinking items entirely
          // (they're shown in the side panel)
          flushThinkingGroup();
        } else {
          // Add to thinking group
          flushToolGroup();
          currentThinkingGroup.push(item);
        }
      } else {
        // Other item - flush any pending groups first
        flushToolGroup();
        flushThinkingGroup();
        result.push(item);
      }
    }
    // Flush any remaining groups
    flushToolGroup();
    flushThinkingGroup();

    return result;
  }, [chatDisplayItems, showThinkingPanel]);

  const runningMissionById = useMemo(() => {
    return new Map(runningMissions.map((m) => [m.mission_id, m]));
  }, [runningMissions]);

  const viewingRunningInfo = useMemo(() => {
    if (!viewingMissionId) return null;
    return runningMissionById.get(viewingMissionId) ?? null;
  }, [runningMissionById, viewingMissionId]);

  const viewingRunState = useMemo<ControlRunState>(() => {
    if (!viewingMissionId) return "idle";
    if (viewingRunningInfo) {
      if (viewingRunningInfo.state === "waiting_for_tool") return "waiting_for_tool";
      if (viewingRunningInfo.state === "queued" || viewingRunningInfo.state === "running") {
        return "running";
      }
      return "idle";
    }
    if (runStateMissionId === viewingMissionId) {
      return runState;
    }
    return "idle";
  }, [viewingMissionId, viewingRunningInfo, runStateMissionId, runState]);

  const viewingQueueLen = useMemo(() => {
    if (!viewingMissionId) return 0;
    if (viewingRunningInfo) return viewingRunningInfo.queue_len;
    if (runStateMissionId === viewingMissionId) return queueLen;
    return 0;
  }, [viewingMissionId, viewingRunningInfo, runStateMissionId, queueLen]);

  const viewingMissionIsRunning = useMemo(() => {
    if (!viewingMissionId) return false;
    if (viewingRunningInfo) {
      return (
        viewingRunningInfo.state === "running" ||
        viewingRunningInfo.state === "waiting_for_tool" ||
        viewingRunningInfo.state === "queued"
      );
    }
    if (runStateMissionId === viewingMissionId) {
      return runState !== "idle";
    }
    return false;
  }, [viewingMissionId, viewingRunningInfo, runStateMissionId, runState]);

  const viewingProgress = useMemo(() => {
    if (!viewingMissionId) return null;
    return progressByMission[viewingMissionId] ?? null;
  }, [progressByMission, viewingMissionId]);

  useEffect(() => {
    if (items.length === 0) return;
    let lastAssistantIdx = -1;
    for (let i = items.length - 1; i >= 0; i--) {
      if (items[i].kind === "assistant") {
        lastAssistantIdx = i;
        break;
      }
    }
    if (lastAssistantIdx === -1) return;
    const visibleStart = Math.max(0, items.length - visibleItemsLimit);
    if (lastAssistantIdx < visibleStart) {
      setVisibleItemsLimit(items.length - lastAssistantIdx);
    }
  }, [items, visibleItemsLimit]);

  const viewingMissionStallInfo = useMemo(() => {
    if (!viewingMissionId) return null;
    if (!viewingRunningInfo) return null;
    if (viewingRunningInfo.health?.status !== "stalled") return null;
    return viewingRunningInfo.health;
  }, [viewingMissionId, viewingRunningInfo]);

  const hasPendingQuestion = useMemo(
    () =>
      items.some(
        (item) =>
          item.kind === "tool" &&
          (item.name === "question" || item.name === "AskUserQuestion") &&
          item.result === undefined
      ),
    [items]
  );

  const viewingMissionStallSeconds = viewingMissionStallInfo?.seconds_since_activity ?? 0;
  const isViewingMissionStalled = Boolean(viewingMissionStallInfo);
  const isViewingMissionSeverelyStalled =
    viewingMissionStallInfo?.severity === "severe";

  const recentMissionList = useMemo(() => {
    if (recentMissions.length === 0) return [];
    const runningIds = new Set(runningMissions.map((m) => m.mission_id));
    const currentId = currentMission?.id ?? null;
    return recentMissions
      .filter((mission) => mission.id !== currentId && !runningIds.has(mission.id))
      .slice(0, 6);
  }, [recentMissions, runningMissions, currentMission?.id]);

  // Treat "waiting_for_tool" as not busy for message input (user should respond immediately)
  const isBusy = viewingRunState === "running";

  const streamCleanupRef = useRef<null | (() => void)>(null);
  const enhancedInputRef = useRef<EnhancedInputHandle>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const viewingMissionIdRef = useRef<string | null>(null);
  const runStateMissionIdRef = useRef<string | null>(null);
  const runningMissionsRef = useRef<RunningMissionInfo[]>([]);
  const currentMissionRef = useRef<Mission | null>(null);
  const viewingMissionRef = useRef<Mission | null>(null);
  const submittingRef = useRef(false); // Guard against double-submission

  // Keep refs in sync with state
  useEffect(() => {
    viewingMissionIdRef.current = viewingMissionId;
  }, [viewingMissionId]);

  useEffect(() => {
    runStateMissionIdRef.current = runStateMissionId;
  }, [runStateMissionId]);

  useEffect(() => {
    runningMissionsRef.current = runningMissions;
  }, [runningMissions]);

  useEffect(() => {
    currentMissionRef.current = currentMission;
  }, [currentMission]);

  useEffect(() => {
    viewingMissionRef.current = viewingMission;
  }, [viewingMission]);

  useEffect(() => {
    itemsRef.current = items;
  }, [items]);

  // Smart auto-scroll
  const { containerRef, endRef, isAtBottom, scrollToBottom, scrollToBottomImmediate } =
    useScrollToBottom();

  // Scroll to bottom synchronously before paint when items change.
  // This ensures the page appears at the bottom instantly when returning
  // to the control page (no visible scroll animation).
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useLayoutEffect(() => {
    if (items.length > 0 && isAtBottom) {
      scrollToBottomImmediate();
    }
  }, [items]);

  // Sync input to localStorage draft
  useEffect(() => {
    setDraftInput(input);
  }, [input, setDraftInput]);

  // Initialize input from draft on mount
  useEffect(() => {
    if (draftInput && !input) {
      setInput(draftInput);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const compressImageFile = useCallback(async (file: File) => {
    if (!file.type.startsWith("image/")) return file;
    if (file.type === "image/gif" || file.type === "image/svg+xml") return file;

    const maxDimension = 1280;
    const minBytesForCompression = 300 * 1024;

    if (file.size < minBytesForCompression) {
      return file;
    }

    let bitmap: ImageBitmap | null = null;
    try {
      bitmap = await createImageBitmap(file);
    } catch {
      return file;
    }

    const maxSide = Math.max(bitmap.width, bitmap.height);
    const scale = Math.min(1, maxDimension / maxSide);
    if (scale === 1 && file.size < minBytesForCompression) {
      bitmap.close();
      return file;
    }

    const targetWidth = Math.max(1, Math.round(bitmap.width * scale));
    const targetHeight = Math.max(1, Math.round(bitmap.height * scale));

    const canvas = document.createElement("canvas");
    canvas.width = targetWidth;
    canvas.height = targetHeight;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      bitmap.close();
      return file;
    }

    ctx.drawImage(bitmap, 0, 0, targetWidth, targetHeight);
    bitmap.close();

    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/jpeg", 0.8)
    );
    if (!blob) return file;

    if (blob.size >= file.size && scale === 1) return file;

    const baseName = file.name.replace(/\.[^.]+$/, "") || "image";
    const compressedName = `${baseName}-compressed.jpg`;
    return new globalThis.File([blob], compressedName, {
      type: "image/jpeg",
      lastModified: Date.now(),
    });
  }, []);

  // Handle file upload - wrapped in useCallback to avoid stale closures
  const handleFileUpload = useCallback(async (file: File, insertion?: InputInsertionState) => {
    let fileToUpload = file;
    try {
      fileToUpload = await compressImageFile(file);
    } catch (error) {
      console.warn("Image compression failed, using original file", error);
    }

    const displayName = fileToUpload.name;
    setUploadQueue((prev) => [...prev, displayName]);
    setUploadProgress({ fileName: displayName, progress: { loaded: 0, total: fileToUpload.size, percentage: 0 } });

    try {
      // Upload to mission-specific context folder if we have a mission
      // Upload into the workspace-local ./context (symlinked to mission context inside the container).
      const contextPath = "./context/";

      // Get workspace_id and mission_id from current or viewing mission
      const mission = viewingMission ?? currentMission;
      const workspaceId = mission?.workspace_id;
      const missionId = mission?.id;

      // Use chunked upload for files > 10MB, regular for smaller
      const useChunked = fileToUpload.size > 10 * 1024 * 1024;

      const result = useChunked
        ? await uploadFileChunked(fileToUpload, contextPath, (progress) => {
            setUploadProgress({ fileName: displayName, progress });
          }, workspaceId, missionId)
        : await uploadFile(fileToUpload, contextPath, (progress) => {
            setUploadProgress({ fileName: displayName, progress });
          }, workspaceId, missionId);

      toast.success(`Uploaded ${result.name}`);

      const uploadNote = `[Uploaded: ${result.path}]`;
      if (insertion) {
        setInput((prev) => {
          const textToInsert = insertion.insertedCount > 0 ? `\n${uploadNote}` : uploadNote;
          const inserted = insertTextAtSelection(prev, textToInsert, {
            start: insertion.start,
            end: insertion.end,
          });
          insertion.start = inserted.cursor;
          insertion.end = inserted.cursor;
          insertion.insertedCount += 1;
          return inserted.value;
        });
      } else {
        // Preserve existing non-paste behavior for attach/upload button paths.
        setInput((prev) => {
          return prev ? `${uploadNote}\n${prev}` : uploadNote;
        });
      }
    } catch (error) {
      console.error("Upload failed:", error);
      toast.error(`Failed to upload ${displayName}`);
    } finally {
      setUploadQueue((prev) => prev.filter((name) => name !== displayName));
      setUploadProgress(null);
    }
  }, [compressImageFile, currentMission, viewingMission]);

  // Handle URL download
  const handleUrlDownload = useCallback(async () => {
    if (!urlInput.trim()) return;

    setUrlDownloading(true);
    try {
      const contextPath = "./context/";

      // Get workspace_id and mission_id from current or viewing mission
      const mission = viewingMission ?? currentMission;
      const workspaceId = mission?.workspace_id;
      const missionId = mission?.id;

      const result = await downloadFromUrl(urlInput.trim(), contextPath, undefined, workspaceId, missionId);
      toast.success(`Downloaded ${result.name}`);

      // Add a message about the download at the beginning (use full path)
      setInput((prev) => {
        const downloadNote = `[Downloaded: ${result.path}]`;
        return prev ? `${downloadNote}\n${prev}` : downloadNote;
      });

      setUrlInput("");
      setShowUrlInput(false);
    } catch (error) {
      console.error("URL download failed:", error);
      toast.error(`Failed to download from URL`);
    } finally {
      setUrlDownloading(false);
    }
  }, [urlInput, currentMission, viewingMission]);


  // Handle file input change
  const handleFileChange = async (
    event: React.ChangeEvent<HTMLInputElement>
  ) => {
    const files = Array.from(event.target.files || []);
    for (const file of files) {
      await handleFileUpload(file);
    }
    // Reset input
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
    }
  };

  // Handle paste to upload files (e.g., screenshots from clipboard)
  const handleFilePaste = useCallback(async (files: File[], context: FilePasteContext) => {
    const insertion: InputInsertionState = {
      start: context.selectionStart,
      end: context.selectionEnd,
      insertedCount: 0,
    };
    for (const file of files) {
      await handleFileUpload(file, insertion);
    }
  }, [handleFileUpload]);

  // Convert mission history to chat items
  const getActiveDesktopSession = useCallback((mission?: Mission | null) => {
    if (!mission || !Array.isArray(mission.desktop_sessions)) {
      return null;
    }
    for (let i = mission.desktop_sessions.length - 1; i >= 0; i -= 1) {
      const session = mission.desktop_sessions[i];
      if (!session?.stopped_at) {
        return session;
      }
    }
    return null;
  }, []);

  const extractDesktopDisplay = useCallback((value: unknown): string | null => {
    function parseDisplayFromString(text: string): string | null {
      try {
        const parsed = JSON.parse(text);
        const nested = extractFromValue(parsed);
        if (nested) return nested;
      } catch {
        // Ignore parse errors - fall back to regex
      }
      const match = text.match(/"display"\s*:\s*"([^"]+)"/i);
      return match ? match[1] : null;
    }

    function extractFromValue(node: unknown): string | null {
      if (!node) return null;
      if (typeof node === "string") {
        return parseDisplayFromString(node);
      }
      if (Array.isArray(node)) {
        for (const item of node) {
          const found = extractFromValue(item);
          if (found) return found;
        }
        return null;
      }
      if (typeof node === "object") {
        const record = node as Record<string, unknown>;
        if (typeof record.display === "string") {
          return record.display;
        }
        if (record.result) {
          const fromResult = extractFromValue(record.result);
          if (fromResult) return fromResult;
        }
        if (record.content) {
          const fromContent = extractFromValue(record.content);
          if (fromContent) return fromContent;
        }
        if (record.structured_content) {
          const fromStructured = extractFromValue(record.structured_content);
          if (fromStructured) return fromStructured;
        }
        if (typeof record.text === "string") {
          const fromText = parseDisplayFromString(record.text);
          if (fromText) return fromText;
        }
      }
      return null;
    }

    return extractFromValue(value);
  }, []);

  // Helper to check if mission history has an active desktop session
  // A session is active if there's a start without a subsequent close
  const missionHasDesktopSession = useCallback(
    (mission: Mission): boolean => {
      if (getActiveDesktopSession(mission)) {
        return true;
      }
      let hasSession = false;
      for (const entry of mission.history) {
        // Check for session start
        if (
          entry.content.includes("desktop_start_session") ||
          entry.content.includes("desktop_desktop_start_session") ||
          entry.content.includes("mcp__desktop__desktop_start_session")
        ) {
          hasSession = true;
        }
        // Check for session close (must come after start check to handle same entry)
        if (
          entry.content.includes("desktop_close_session") ||
          entry.content.includes("desktop_desktop_close_session") ||
          entry.content.includes("mcp__desktop__desktop_close_session")
        ) {
          hasSession = false;
        }
      }
      return hasSession;
    },
    [getActiveDesktopSession]
  );

  const applyDesktopSessionState = useCallback(
    (mission: Mission) => {
      const activeSession = getActiveDesktopSession(mission);
      if (activeSession?.display) {
        // Only switch display if the current one is not running for THIS mission.
        // This prevents auto-switching away from a display the user is actively viewing,
        // but allows switching when changing to a different mission.
        const currentDisplayId = desktopDisplayIdRef.current;
        const currentBelongsToThisMission = mission.desktop_sessions?.some(
          s => s.display === currentDisplayId && !s.stopped_at
        );
        if (!currentBelongsToThisMission) {
          setDesktopDisplayId(activeSession.display);
        }
        setHasDesktopSession(true);
        // Auto-open desktop panel when mission has an active session
        setShowDesktopStream(true);
        return;
      }
      if (missionHasDesktopSession(mission)) {
        setHasDesktopSession(true);
        setShowDesktopStream(true);
      } else {
        setHasDesktopSession(false);
      }
    },
    [getActiveDesktopSession, missionHasDesktopSession]
  );

  // Detect desktop sessions from stored events (when loading from history)
  // This handles the case where mission.desktop_sessions isn't populated yet
  // and mission.history doesn't include tool calls (SQLite only stores user/assistant messages)
  const applyDesktopSessionFromEvents = useCallback(
    (events: StoredEvent[] | null) => {
      if (!events) return;

      // Track sessions by display: true = started, false = closed
      const sessionsByDisplay = new Map<string, boolean>();
      let latestActiveDisplay: string | null = null;

      for (const event of events) {
        if (event.event_type !== "tool_result") continue;

        const toolName = event.tool_name;
        const isStart =
          toolName === "desktop_start_session" ||
          toolName === "desktop_desktop_start_session" ||
          toolName === "mcp__desktop__desktop_start_session";
        const isClose =
          toolName === "desktop_close_session" ||
          toolName === "desktop_desktop_close_session" ||
          toolName === "mcp__desktop__desktop_close_session";

        if (!isStart && !isClose) continue;

        // Parse result to get display
        const display = extractDesktopDisplay(event.content);
        if (!display) continue;

        if (isStart) {
          sessionsByDisplay.set(display, true);
          latestActiveDisplay = display;
        } else if (isClose) {
          sessionsByDisplay.set(display, false);
          if (latestActiveDisplay === display) {
            latestActiveDisplay = null;
          }
        }
      }

      // Check if we found any active sessions
      if (latestActiveDisplay) {
        setDesktopDisplayId(latestActiveDisplay);
        setHasDesktopSession(true);
        setShowDesktopStream(true);
      } else {
        // Check if any session is still active
        for (const [display, isActive] of sessionsByDisplay) {
          if (isActive) {
            setDesktopDisplayId(display);
            setHasDesktopSession(true);
            setShowDesktopStream(true);
            return;
          }
        }
      }
    },
    [extractDesktopDisplay]
  );

  const hasRunningDesktopSessionForMission = useCallback(
    (missionId: string | null): boolean => {
      if (!missionId) return false;
      const activeMission =
        viewingMissionRef.current ?? currentMissionRef.current;
      if (activeMission?.id === missionId) {
        if (getActiveDesktopSession(activeMission)) {
          return true;
        }
      }
      return desktopSessionsRef.current.some(
        (session) =>
          session.process_running &&
          session.status !== "stopped" &&
          session.mission_id === missionId
      );
    },
    [getActiveDesktopSession]
  );

  const missionForDownloads = viewingMission ?? currentMission;

  // Derive working directory from mission's desktop sessions for file path resolution
  const missionWorkingDirectory = useMemo(() => {
    const mission = missionForDownloads;
    if (!mission) return undefined;

    if (mission.desktop_sessions?.length) {
      // Try to find screenshots_dir from any session (prefer active/latest)
      for (let i = mission.desktop_sessions.length - 1; i >= 0; i--) {
        const session = mission.desktop_sessions[i];
        if (session?.screenshots_dir) {
          // screenshots_dir is like /path/to/workspace/screenshots/
          // We want the parent: /path/to/workspace/
          const dir = session.screenshots_dir.replace(/\/?$/, ""); // remove trailing slash
          const parent = dir.substring(0, dir.lastIndexOf("/"));
          if (parent) return parent;
        }
      }
    }

    const workspace =
      workspaces.find((ws) => ws.id === mission.workspace_id) ??
      workspaces.find((ws) => ws.workspace_type === "host");

    if (!workspace?.path) return undefined;
    const cleanRoot = workspace.path.replace(/\/+$/, "");
    // Per-mission workspace dir matches backend convention:
    // `{workspace.path}/workspaces/mission-{mission_id_prefix}`.
    //
    // This lets rich `<image>`/`<file>` tags using relative paths (e.g. `./chart.svg`)
    // resolve correctly even when no desktop session was started.
    const shortId = mission.id?.slice(0, 8);
    if (shortId) {
      return `${cleanRoot}/workspaces/mission-${shortId}`;
    }
    return cleanRoot;
  }, [missionForDownloads, workspaces]);

  const missionHistoryToItems = useCallback((mission: Mission): ChatItem[] => {
    // Estimate timestamps based on mission creation time
    const baseTime = new Date(mission.created_at).getTime();
    // Find index of last assistant message to apply mission status
    const lastAssistantIdx = mission.history.reduce(
      (lastIdx, entry, i) => (entry.role === "assistant" ? i : lastIdx),
      -1
    );
    // Mission is considered failed if status is "failed"
    const missionFailed = mission.status === "failed";

    return mission.history.map((entry, i) => {
      // Spread timestamps across history (rough estimate)
      const timestamp = baseTime + i * 60000; // 1 minute apart
      if (entry.role === "user") {
        return {
          kind: "user" as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
          timestamp,
        };
      } else {
        // Last assistant message inherits mission status
        // Earlier assistant messages are assumed successful
        const isLastAssistant = i === lastAssistantIdx;
        const success = isLastAssistant ? !missionFailed : true;
        return {
          kind: "assistant" as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
          success,
          costCents: 0,
          costSource: "unknown" as const,
          model: null,
          timestamp,
          resumable: isLastAssistant && missionFailed ? mission.resumable : undefined,
        };
      }
    });
  }, []);

  // Convert stored events (from SQLite) to ChatItems for display
  // This enables full history replay including tool calls on page refresh
  const eventsToItems = useCallback((events: StoredEvent[], mission?: Mission | null): ChatItem[] => {
    const items: ChatItem[] = [];
    const toolCallMap = new Map<string, number>(); // tool_call_id -> index in items
    // Track seen event IDs to prevent duplicate items (backend may store duplicates)
    const seenEventIds = new Set<string>();
    // Track current in-progress thinking item index for consolidation
    // Multiple thinking events (deltas) are streamed and stored; we consolidate them here
    let currentThinkingIdx: number | null = null;
    let lastTextDelta:
      | { id: string; content: string; timestamp: number }
      | null = null;
    let lastAssistantTimestamp = 0;
    const missionActive = mission?.status === "active";

    // Helper to finalize pending thinking item
    const finalizePendingThinking = (endTime: number) => {
      if (currentThinkingIdx !== null) {
        const pending = items[currentThinkingIdx] as Extract<ChatItem, { kind: "thinking" }>;
        if (!pending.done) {
          items[currentThinkingIdx] = {
            ...pending,
            done: true,
            endTime,
          };
        }
        currentThinkingIdx = null;
      }
    };

    for (const event of events) {
      const timestamp = new Date(event.timestamp).getTime();

      switch (event.event_type) {
        case "user_message": {
          // Finalize any pending thinking before user message
          finalizePendingThinking(timestamp);
          // Use event_id (UUID) if available for deduplication with SSE events,
          // fall back to row id for older events without event_id
          const itemId = event.event_id ?? `event-${event.id}`;
          // Skip duplicate events (backend may store the same event multiple times)
          if (seenEventIds.has(itemId)) break;
          seenEventIds.add(itemId);
          items.push({
            kind: "user" as const,
            id: itemId,
            content: event.content,
            timestamp,
          });
          break;
        }

        case "assistant_message": {
          // Finalize any pending thinking before assistant message
          finalizePendingThinking(timestamp);
          const meta = event.metadata || {};
          const isFailure = meta.success === false;
          const { costCents, costSource } = parseCostMetadata(meta);

          // When mission fails, mark all pending tool calls as failed
          // This ensures subagent headers don't stay stuck showing "Running for X"
          if (isFailure) {
            const errorMessage = event.content || "Mission failed";
            for (let i = 0; i < items.length; i++) {
              const it = items[i];
              if (it.kind === "tool" && it.result === undefined) {
                items[i] = {
                  ...it,
                  result: { error: errorMessage, status: "failed" },
                  endTime: timestamp,
                };
              }
            }
          }

          // Use event_id (UUID) if available for deduplication with SSE events
          const assistantId = event.event_id ?? `event-${event.id}`;
          // Skip duplicate events
          if (seenEventIds.has(assistantId)) break;
          seenEventIds.add(assistantId);
          items.push({
            kind: "assistant" as const,
            id: assistantId,
            content: event.content,
            success: !isFailure,
            costCents,
            costSource,
            model: typeof meta.model === "string" ? meta.model : null,
            timestamp,
          });
          lastAssistantTimestamp = timestamp;
          break;
        }

        case "text_delta": {
          const content = event.content || "";
          if (content.trim().length === 0) break;
          lastTextDelta = {
            id: event.event_id ?? `text-delta-${event.id}`,
            content,
            timestamp,
          };
          break;
        }

        case "thinking": {
          // Consolidate thinking events: backend streams multiple deltas that we merge
          const meta = event.metadata || {};
          const isDone = meta.done === true;
          const content = event.content || "";

          if (currentThinkingIdx !== null) {
            const existing = items[currentThinkingIdx] as Extract<ChatItem, { kind: "thinking" }>;
            const existingContent = existing.content || "";
            const isContinuation =
              !content ||
              !existingContent ||
              content.startsWith(existingContent) ||
              existingContent.startsWith(content);

            if (!isContinuation) {
              // Treat as a new thought session: finalize previous and start a new item.
              items[currentThinkingIdx] = {
                ...existing,
                done: true,
                endTime: timestamp,
              };
              const newIdx = items.length;
              items.push({
                kind: "thinking" as const,
                id: `event-${event.id}`,
                content,
                done: isDone,
                startTime: timestamp,
                endTime: isDone ? timestamp : undefined,
              });
              currentThinkingIdx = isDone ? null : newIdx;
            } else {
              // Continuation of the same thought: keep the longer content.
              const newContent = content.length > existingContent.length ? content : existingContent;
              items[currentThinkingIdx] = {
                ...existing,
                content: newContent,
                done: isDone,
                endTime: isDone ? timestamp : existing.endTime,
              };
              if (isDone) {
                currentThinkingIdx = null; // Reset for next thinking session
              }
            }
          } else {
            const newIdx = items.length;
            items.push({
              kind: "thinking" as const,
              id: `event-${event.id}`,
              content,
              done: isDone,
              startTime: timestamp,
              endTime: isDone ? timestamp : undefined,
            });
            if (!isDone) {
              currentThinkingIdx = newIdx; // Track for consolidation
            }
          }
          break;
        }

        case "tool_call": {
          // Finalize any pending thinking before tool call
          finalizePendingThinking(timestamp);
          // Clear pending text delta — the text before this tool call belongs to
          // the same assistant turn, so it shouldn't linger as a separate "Draft"
          // stream item if the tool call hangs or the assistant_message is never emitted.
          lastTextDelta = null;
          const toolCallId = event.tool_call_id || `unknown-${event.id}`;
          const name = event.tool_name || "unknown";
          const isUiTool = name.startsWith("ui_") || name === "question" || name === "AskUserQuestion";
          // Parse args from content (stored as JSON string)
          let args: unknown = undefined;
          try {
            args = event.content ? JSON.parse(event.content) : undefined;
          } catch {
            args = event.content;
          }
          const toolItem: ChatItem = {
            kind: "tool" as const,
            id: `tool-${toolCallId}`,
            toolCallId,
            name,
            args,
            isUiTool,
            startTime: timestamp,
            result: undefined,
            endTime: undefined,
          };
          toolCallMap.set(toolCallId, items.length);
          items.push(toolItem);
          break;
        }

        case "tool_result": {
          const toolCallId = event.tool_call_id || "";
          const idx = toolCallMap.get(toolCallId);
          if (idx !== undefined) {
            // Update existing tool item with result
            const toolItem = items[idx] as Extract<ChatItem, { kind: "tool" }>;
            // Parse result from content
            let result: unknown = event.content;
            try {
              result = event.content ? JSON.parse(event.content) : event.content;
            } catch {
              // Keep as string if not valid JSON
            }
            items[idx] = {
              ...toolItem,
              result,
              endTime: timestamp,
            };
          }
          break;
        }

        // Skip other event types (error, mission_status_changed, etc.)
      }
    }

    // Finalize any pending thinking item (e.g., if done event wasn't stored).
    // For active missions, keep the last item "open" so the side panel can show ongoing progress.
    if (!missionActive) {
      finalizePendingThinking(Date.now());
    }

    if (lastTextDelta && lastTextDelta.timestamp > lastAssistantTimestamp) {
      // If the text delta is stale (>5 min old) and mission is still active,
      // treat it as finalized to avoid showing a permanent "Draft" label
      // when a tool call hung or the assistant_message event was lost.
      const STALE_THRESHOLD_MS = 5 * 60 * 1000;
      const isStale =
        missionActive && Date.now() - lastTextDelta.timestamp > STALE_THRESHOLD_MS;
      const isDone = !missionActive || isStale;
      items.push({
        kind: "stream" as const,
        id: lastTextDelta.id,
        content: lastTextDelta.content,
        done: isDone,
        startTime: lastTextDelta.timestamp,
        endTime: isDone ? lastTextDelta.timestamp : undefined,
      });
    }

    return items;
  }, []);

  // Load mission from URL param on mount (and retry on auth success)
  const [authRetryTrigger, setAuthRetryTrigger] = useState(0);

  // Listen for auth success to retry loading
  useEffect(() => {
    const onAuthSuccess = () => {
      setAuthRetryTrigger((prev) => prev + 1);
    };
    window.addEventListener("openagent:auth:success", onAuthSuccess);
    return () => window.removeEventListener("openagent:auth:success", onAuthSuccess);
  }, []);

  useEffect(() => {
    let cancelled = false;
      const missionId = searchParams.get("mission");

      const loadFromQuery = async (id: string) => {
        const pendingId = pendingMissionNavRef.current;
        if (pendingId && id !== pendingId) {
          // Ignore stale query params while we navigate to a newly-created mission.
          return;
        }
        if (pendingId && id === pendingId) {
          pendingMissionNavRef.current = null;
        }
        // Skip loading if we already have this mission in state (e.g., after handleNewMission)
        if (viewingMissionRef.current?.id === id) {
          setViewingMissionId(id);
          return;
        }
        // Skip if handleViewMission is already loading this mission (prevents double-load race)
        if (handleViewMissionLoadingRef.current === id) {
          return;
        }
      const previousViewingMission = viewingMissionRef.current;
      setMissionLoading(true);
      setViewingMissionId(id); // Set viewing ID immediately to prevent "Agent is working..." flash
      fetchingMissionIdRef.current = id; // Track which mission we're loading
      try {
        // Load mission, events, and queue in parallel for faster load
        const [mission, events, queuedMessages] = await Promise.all([
          loadMission(id),
          loadHistoryEvents(id).catch(() => null), // Don't fail if events unavailable
          getQueue().catch(() => []), // Don't fail if queue unavailable
        ]);
        if (cancelled || fetchingMissionIdRef.current !== id) return;
        // Mission not found (404) - clear state and URL param without showing error
        if (!mission) {
          setViewingMissionId(null);
          setViewingMission(null);
          setCurrentMission(null);
          setItems([]);
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          setHasDesktopSession(false);
          setLastMissionId(null); // Clear stale last mission ID from localStorage
          router.replace("/control", { scroll: false });
          return;
        }
        setCurrentMission(mission);
        setViewingMission(mission);
        // Use events if available, otherwise fall back to basic history
        let historyItems = events ? eventsToItems(events, mission) : missionHistoryToItems(mission);
        if (events && !historyItems.some((item) => item.kind === "assistant")) {
          const historyHasAssistant = mission.history.some(
            (entry) => entry.role === "assistant"
          );
          if (historyHasAssistant) {
            historyItems = missionHistoryToItems(mission);
          }
        }
        // Merge queued messages that belong to this mission
        const missionQueuedMessages = queuedMessages.filter((qm) => qm.mission_id === id);
        if (missionQueuedMessages.length > 0) {
          const queuedIds = new Set(missionQueuedMessages.map((qm) => qm.id));
          // Mark existing items as queued
          historyItems = historyItems.map((item) =>
            item.kind === "user" && queuedIds.has(item.id) ? { ...item, queued: true } : item
          );
          // Add any queued messages not already in history
          const existingIds = new Set(historyItems.map((item) => item.id));
          const newQueuedItems: ChatItem[] = missionQueuedMessages
            .filter((qm) => !existingIds.has(qm.id))
            .map((qm) => ({
              kind: "user" as const,
              id: qm.id,
              content: qm.content,
              timestamp: Date.now(),
              agent: qm.agent ?? undefined,
              queued: true,
            }));
          historyItems = [...historyItems, ...newQueuedItems];
        }
        setItems(historyItems);
        adjustVisibleItemsLimit(historyItems);
        applyDesktopSessionState(mission);
        // Also check events for desktop sessions (in case mission.desktop_sessions isn't populated yet)
        if (events) {
          applyDesktopSessionFromEvents(events);
        }
      } catch (err) {
        if (cancelled || fetchingMissionIdRef.current !== id) return;
        console.error("Failed to load mission:", err);
        // Show error toast for mission load failures (skip if likely a 401 during initial page load)
        const is401 = (err as Error)?.message?.includes("401") || (err as { status?: number })?.status === 401;
        if (!is401) {
          toast.error("Failed to load mission");
        }

        // Revert viewing state to the previous mission to avoid filtering out events
        const fallbackMission = previousViewingMission ?? currentMissionRef.current;
        if (fallbackMission) {
          setViewingMissionId(fallbackMission.id);
          setViewingMission(fallbackMission);
          setItems(missionHistoryToItems(fallbackMission));
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          applyDesktopSessionState(fallbackMission);
        } else {
          setViewingMissionId(null);
          setViewingMission(null);
          setItems([]);
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          setHasDesktopSession(false);
        }
      } finally {
        if (!cancelled) setMissionLoading(false);
      }
    };

    const loadFromCurrent = async () => {
      try {
        const mission = await getCurrentMission();
        if (cancelled) return;
        if (mission) {
          setCurrentMission(mission);
          setViewingMission(mission);
          // Show basic history immediately, then load full events
          {
            const basicItems = missionHistoryToItems(mission);
            setItems(basicItems);
            adjustVisibleItemsLimit(basicItems);
          }
          applyDesktopSessionState(mission);
          router.replace(`/control?mission=${mission.id}`, { scroll: false });
          // Load full events and queue in background (including tool calls)
          Promise.all([loadHistoryEvents(mission.id), getQueue().catch(() => [])])
            .then(([events, queuedMessages]) => {
              if (cancelled) return;
              let historyItems = eventsToItems(events, mission);
              if (!historyItems.some((item) => item.kind === "assistant")) {
                const historyHasAssistant = mission.history.some(
                  (entry) => entry.role === "assistant"
                );
                if (historyHasAssistant) {
                  historyItems = missionHistoryToItems(mission);
                }
              }
              // Merge queued messages that belong to this mission
              const missionQueuedMessages = queuedMessages.filter((qm) => qm.mission_id === mission.id);
              if (missionQueuedMessages.length > 0) {
                const queuedIds = new Set(missionQueuedMessages.map((qm) => qm.id));
                historyItems = historyItems.map((item) =>
                  item.kind === "user" && queuedIds.has(item.id) ? { ...item, queued: true } : item
                );
                const existingIds = new Set(historyItems.map((item) => item.id));
                const newQueuedItems: ChatItem[] = missionQueuedMessages
                  .filter((qm) => !existingIds.has(qm.id))
                  .map((qm) => ({
                    kind: "user" as const,
                    id: qm.id,
                    content: qm.content,
                    timestamp: Date.now(),
                    agent: qm.agent ?? undefined,
                    queued: true,
                  }));
                historyItems = [...historyItems, ...newQueuedItems];
              }
              setItems(historyItems);
              adjustVisibleItemsLimit(historyItems);
              // Also check events for desktop sessions
              applyDesktopSessionFromEvents(events);
            })
            .catch(() => {}); // Keep basic history on failure
          return;
        }

        if (lastMissionId) {
          await loadFromQuery(lastMissionId);
        }
      } catch (err) {
        if (!cancelled) {
          console.error("Failed to get current mission:", err);
        }
      }
    };

    if (missionId) {
      loadFromQuery(missionId);
    } else {
      loadFromCurrent();
    }

    return () => {
      cancelled = true;
    };
  }, [
    searchParams,
    router,
    missionHistoryToItems,
    adjustVisibleItemsLimit,
    loadHistoryEvents,
    applyDesktopSessionState,
    applyDesktopSessionFromEvents,
    authRetryTrigger,
    setLastMissionId,
  ]);

  useEffect(() => {
    const id = viewingMission?.id ?? currentMission?.id;
    if (!id) return;
    setLastMissionId((prev) => (prev === id ? prev : id));
  }, [viewingMission?.id, currentMission?.id, setLastMissionId]);

  // Poll for running parallel missions
  useEffect(() => {
    const pollRunning = async () => {
      try {
        const running = await getRunningMissions();
        setRunningMissions(running);
      } catch {
        // Ignore errors
      }
    };

    // Poll immediately and then every 3 seconds
    pollRunning();
    const interval = setInterval(pollRunning, 3000);
    return () => clearInterval(interval);
  }, []);

  const refreshRecentMissions = useCallback(async () => {
    try {
      const missions = await listMissions();
      setRecentMissions(missions);
    } catch (err) {
      if (isNetworkError(err)) return;
      console.error("Failed to fetch missions:", err);
    }
  }, []);

  const handleStreamDiagnostics = useCallback((update: StreamDiagnosticUpdate) => {
    switch (update.phase) {
      case "connecting":
        streamLog("info", "connecting", { url: update.url });
        break;
      case "open":
        streamLog("info", "open", {
          url: update.url,
          status: update.status,
          headers: update.headers,
        });
        break;
      case "chunk":
        streamLog("debug", "chunk", { url: update.url, bytes: update.bytes });
        break;
      case "event":
        streamLog("debug", "event", { url: update.url, bytes: update.bytes });
        break;
      case "closed":
        streamLog("warn", "closed", { url: update.url, bytes: update.bytes });
        break;
      case "error":
        streamLog("error", "error", {
          url: update.url,
          status: update.status,
          error: update.error,
        });
        break;
    }

    setStreamDiagnostics((prev) => {
      const next: StreamDiagnosticsState = { ...prev };
      if (update.url) next.url = update.url;

      switch (update.phase) {
        case "connecting":
          next.phase = "connecting";
          next.lastError = null;
          next.bytes = 0;
          next.status = undefined;
          next.contentType = undefined;
          next.cacheControl = undefined;
          next.transferEncoding = undefined;
          next.contentEncoding = undefined;
          next.server = undefined;
          next.via = undefined;
          next.lastEventAt = undefined;
          next.lastChunkAt = undefined;
          break;
        case "open":
          next.phase = "open";
          next.status = update.status;
          if (update.headers) {
            next.contentType = update.headers["content-type"] ?? null;
            next.cacheControl = update.headers["cache-control"] ?? null;
            next.transferEncoding = update.headers["transfer-encoding"] ?? null;
            next.contentEncoding = update.headers["content-encoding"] ?? null;
            next.server = update.headers["server"] ?? null;
            next.via = update.headers["via"] ?? null;
          }
          break;
        case "chunk":
          next.phase = next.phase === "error" ? "error" : "streaming";
          next.lastChunkAt = update.timestamp;
          if (typeof update.bytes === "number") next.bytes = update.bytes;
          break;
        case "event":
          next.phase = next.phase === "error" ? "error" : "streaming";
          next.lastEventAt = update.timestamp;
          if (typeof update.bytes === "number") next.bytes = update.bytes;
          break;
        case "closed":
          next.phase = "closed";
          break;
        case "error":
          next.phase = "error";
          next.lastError = update.error ?? next.lastError ?? "Stream error";
          if (typeof update.bytes === "number") next.bytes = update.bytes;
          if (typeof update.status === "number") next.status = update.status;
          break;
      }

      return next;
    });
  }, []);

  // Refresh recent missions periodically (after the callback is defined)
  useEffect(() => {
    refreshRecentMissions();
    const interval = setInterval(refreshRecentMissions, 10000);
    return () => clearInterval(interval);
  }, [refreshRecentMissions]);

  // Fetch desktop sessions periodically for the enhanced dropdown
  const refreshDesktopSessions = useCallback(async () => {
    try {
      const sessions = await listDesktopSessions();
      setDesktopSessions(sessions);
      // Find running sessions
      const runningSessions = sessions.filter(s => s.process_running && s.status !== 'stopped');
      const hasRunning = runningSessions.length > 0;

      if (hasRunning) {
        // Get current mission ID to scope auto-open behavior
        const activeMission = viewingMissionRef.current ?? currentMissionRef.current;
        const activeMissionId = activeMission?.id;

        // Only auto-open for sessions belonging to the current mission.
        // When expecting a desktop session (ToolCall detected but no ToolResult yet),
        // also include unattributed sessions (mission_id is null) since the backend
        // background task may not have attributed them yet.
        const expecting = expectingDesktopSessionRef.current;
        const currentMissionSessions = activeMissionId
          ? runningSessions.filter(s =>
              s.mission_id === activeMissionId || (expecting && !s.mission_id)
            )
          : expecting
            ? runningSessions.filter(s => !s.mission_id)
            : [];
        const hasCurrentMissionSession = currentMissionSessions.length > 0;

        // Auto-select first active session from current mission if current display isn't running anywhere
        if (hasCurrentMissionSession) {
          const currentIsRunningAnywhere = runningSessions.some(s => s.display === desktopDisplayId);
          if (!currentIsRunningAnywhere) {
            setDesktopDisplayId(currentMissionSessions[0].display);
          }
          // Auto-open desktop panel only when there's an active session for the current mission
          if (!hasDesktopSession) {
            setHasDesktopSession(true);
            setShowDesktopStream(true);
          }
          // Clear expecting flag once we found a session
          if (expecting) {
            expectingDesktopSessionRef.current = false;
            if (desktopRapidPollRef.current) {
              clearInterval(desktopRapidPollRef.current);
              desktopRapidPollRef.current = null;
            }
          }
        }
      }
    } catch (err) {
      if (isNetworkError(err)) return;
      // Silently fail - desktop sessions are optional
    }
  }, [hasDesktopSession, desktopDisplayId]);

  useEffect(() => {
    refreshDesktopSessions();
    const interval = setInterval(refreshDesktopSessions, 10000);
    return () => {
      clearInterval(interval);
      // Also clean up rapid polling interval
      if (desktopRapidPollRef.current) {
        clearInterval(desktopRapidPollRef.current);
        desktopRapidPollRef.current = null;
      }
    };
  }, [refreshDesktopSessions]);

  // Handle closing a desktop session
  const handleCloseDesktopSession = useCallback(async (display: string) => {
    setIsClosingDesktop(display);
    try {
      await closeDesktopSession(display);
      toast.success(`Desktop session ${display} closed`);
      // Refresh sessions
      await refreshDesktopSessions();
      // If we closed the currently viewed display, switch to another or hide
      if (desktopDisplayId === display) {
        const remaining = desktopSessions.filter(s => s.display !== display && s.process_running);
        if (remaining.length > 0) {
          setDesktopDisplayId(remaining[0].display);
        } else {
          setShowDesktopStream(false);
          setHasDesktopSession(false);
        }
      }
    } catch (err) {
      toast.error(`Failed to close session: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setIsClosingDesktop(null);
    }
  }, [desktopDisplayId, desktopSessions, refreshDesktopSessions]);

  // Handle extending keep-alive
  const handleKeepAliveDesktopSession = useCallback(async (display: string) => {
    try {
      await keepAliveDesktopSession(display, 7200); // 2 hours
      toast.success(`Keep-alive extended for ${display}`);
      await refreshDesktopSessions();
    } catch (err) {
      toast.error(`Failed to extend keep-alive: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  }, [refreshDesktopSessions]);

  // Global keyboard shortcut for mission switcher (Cmd+K / Ctrl+K)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        setShowMissionSwitcher(true);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, []);

  // Fetch workspaces and agents for mission creation
  useEffect(() => {
    listWorkspaces()
      .then((data) => {
        setWorkspaces(data);
      })
      .catch((err) => {
        if (isNetworkError(err)) return;
        console.error("Failed to fetch workspaces:", err);
      });
  }, [authRetryTrigger]);

  // Fetch server configuration (max_iterations) from health endpoint
  useEffect(() => {
    getHealth()
      .then((data) => {
        if (data.max_iterations) {
          setMaxIterations(data.max_iterations);
        }
      })
      .catch((err) => {
        if (isNetworkError(err)) return;
        console.error("Failed to fetch health:", err);
      });
  }, []);

  // Handle cancelling a parallel mission
  const handleCancelMission = async (missionId: string) => {
    try {
      await cancelMission(missionId);
      toast.success("Mission cancelled");
      // Refresh running list
      const running = await getRunningMissions();
      setRunningMissions(running);
    } catch (err) {
      console.error("Failed to cancel mission:", err);
      toast.error("Failed to cancel mission");
    }
  };

  // Track the mission ID being fetched to prevent race conditions
  const fetchingMissionIdRef = useRef<string | null>(null);
  const pendingMissionNavRef = useRef<string | null>(null);
  const handleViewMissionLoadingRef = useRef<string | null>(null);

  // Handle switching which mission we're viewing
  const handleViewMission = useCallback(
    async (missionId: string) => {
      const previousViewingId = viewingMissionIdRef.current;
      const previousViewingMission = viewingMissionRef.current;

      // Clear pending thinking state to prevent stale content from appearing in new mission
      if (thinkingFlushTimeoutRef.current) {
        clearTimeout(thinkingFlushTimeoutRef.current);
        thinkingFlushTimeoutRef.current = null;
      }
      pendingThinkingRef.current = null;
      if (streamFlushTimeoutRef.current) {
        clearTimeout(streamFlushTimeoutRef.current);
        streamFlushTimeoutRef.current = null;
      }
      pendingStreamRef.current = null;

      setViewingMissionId(missionId);
      fetchingMissionIdRef.current = missionId;
      handleViewMissionLoadingRef.current = missionId;

      // Update URL immediately so it's shareable/bookmarkable
      router.replace(`/control?mission=${missionId}`, { scroll: false });

      // Always load fresh history from API when switching missions
      // This ensures we don't show stale cached events
      try {
        // Load mission, events, and queue in parallel for faster load
        const [mission, events, queuedMessages] = await Promise.all([
          getMission(missionId),
          loadHistoryEvents(missionId).catch(() => null), // Don't fail if events unavailable
          getQueue().catch(() => []), // Don't fail if queue unavailable
        ]);

        // Race condition guard: only update if this is still the mission we want
        if (fetchingMissionIdRef.current !== missionId) {
          return; // Another mission was requested, discard this response
        }

        // Use events if available, otherwise fall back to basic history
        let historyItems = events ? eventsToItems(events, mission) : missionHistoryToItems(mission);
        if (events && !historyItems.some((item) => item.kind === "assistant")) {
          const historyHasAssistant = mission.history.some(
            (entry) => entry.role === "assistant"
          );
          if (historyHasAssistant) {
            historyItems = missionHistoryToItems(mission);
          }
        }

        // Merge queued messages that belong to this mission
        const missionQueuedMessages = queuedMessages.filter((qm) => qm.mission_id === missionId);
        if (missionQueuedMessages.length > 0) {
          const queuedIds = new Set(missionQueuedMessages.map((qm) => qm.id));
          historyItems = historyItems.map((item) =>
            item.kind === "user" && queuedIds.has(item.id) ? { ...item, queued: true } : item
          );
          const existingIds = new Set(historyItems.map((item) => item.id));
          const newQueuedItems: ChatItem[] = missionQueuedMessages
            .filter((qm) => !existingIds.has(qm.id))
            .map((qm) => ({
              kind: "user" as const,
              id: qm.id,
              content: qm.content,
              timestamp: Date.now(),
              agent: qm.agent ?? undefined,
              queued: true,
            }));
          historyItems = [...historyItems, ...newQueuedItems];
        }

        setItems(historyItems);
        adjustVisibleItemsLimit(historyItems);
        // Check if mission has an active desktop session (stored metadata or fallback to history)
        applyDesktopSessionState(mission);
        // Also check events for desktop sessions
        if (events) {
          applyDesktopSessionFromEvents(events);
        }
        // Update cache with fresh data (with LRU cleanup)
        updateMissionItems(missionId, historyItems);
        setViewingMission(mission);
        if (currentMissionRef.current?.id === mission.id) {
          setCurrentMission(mission);
        }
        handleViewMissionLoadingRef.current = null;
      } catch (err) {
        console.error("Failed to load mission:", err);
        handleViewMissionLoadingRef.current = null;

        // Race condition guard: only update if this is still the mission we want
        if (fetchingMissionIdRef.current !== missionId) {
          return;
        }

        // Revert viewing state to avoid filtering out events
        const fallbackMission = previousViewingMission ?? currentMissionRef.current;
        if (fallbackMission) {
          setViewingMissionId(fallbackMission.id);
          setViewingMission(fallbackMission);
          setItems(missionHistoryToItems(fallbackMission));
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          applyDesktopSessionState(fallbackMission);
          router.replace(`/control?mission=${fallbackMission.id}`, { scroll: false });
        } else if (previousViewingId && missionItems[previousViewingId]) {
          setViewingMissionId(previousViewingId);
          setViewingMission(null);
          setItems(missionItems[previousViewingId]);
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          router.replace(`/control?mission=${previousViewingId}`, { scroll: false });
        } else {
          setViewingMissionId(null);
          setViewingMission(null);
          setItems([]);
          setVisibleItemsLimit(INITIAL_VISIBLE_ITEMS);
          setHasDesktopSession(false);
          router.replace(`/control`, { scroll: false });
        }
      }
    },
    [
      missionItems,
      missionHistoryToItems,
      eventsToItems,
      applyDesktopSessionState,
      adjustVisibleItemsLimit,
      loadHistoryEvents,
      router,
    ]
  );

  // Sync viewingMissionId with currentMission only when there's no explicit viewing mission set
  useEffect(() => {
    if (currentMission && !viewingMissionId) {
      setViewingMissionId(currentMission.id);
      setViewingMission(currentMission);
    } else if (currentMission && viewingMissionId === currentMission.id) {
      // Only update viewingMission if we're actually viewing the current mission
      setViewingMission(currentMission);
    }
  }, [currentMission, viewingMissionId]);

  // Note: We don't auto-cache items from SSE events because they may not have mission_id
  // and could be from any mission. We only cache when explicitly loading from API.

  // Handle creating a new mission
  // Returns the mission ID for the NewMissionDialog to handle navigation
  const handleNewMission = async (options?: {
    workspaceId?: string;
    agent?: string;
    modelOverride?: string;
    modelEffort?: "low" | "medium" | "high";
    configProfile?: string;
    backend?: string;
    openInNewTab?: boolean;
  }) => {
    try {
      setMissionLoading(true);
      const mission = await createMission({
        workspaceId: options?.workspaceId,
        agent: options?.agent,
        modelOverride: options?.modelOverride,
        modelEffort: options?.modelEffort,
        configProfile: options?.configProfile,
        backend: options?.backend,
      });

      // Only update local state for same-tab navigation
      // For new tab, the new tab will load its own state
      if (!options?.openInNewTab) {
        pendingMissionNavRef.current = mission.id;
        setCurrentMission(mission);
        setViewingMission(mission);
        setViewingMissionId(mission.id);
        setItems([]);
        setHasDesktopSession(false);
      }

      // Refresh running missions to get accurate state
      const running = await getRunningMissions();
      setRunningMissions(running);
      refreshRecentMissions();
      toast.success("New mission created");
      // Return ID for dialog to handle navigation
      return { id: mission.id };
    } catch (err) {
      console.error("Failed to create mission:", err);
      toast.error("Failed to create new mission");
      throw err; // Re-throw so dialog knows creation failed
    } finally {
      setMissionLoading(false);
    }
  };

  // Handle setting mission status
  const handleSetStatus = async (status: MissionStatus) => {
    const mission = viewingMission ?? currentMission;
    if (!mission) return;
    try {
      await setMissionStatus(mission.id, status);
      if (currentMission?.id === mission.id) {
        setCurrentMission({ ...mission, status });
      }
      if (viewingMission?.id === mission.id) {
        setViewingMission({ ...mission, status });
      }
      refreshRecentMissions();
      toast.success(`Mission marked as ${status}`);
    } catch (err) {
      console.error("Failed to set mission status:", err);
      toast.error("Failed to update mission status");
    }
  };

  // Handle resuming an interrupted mission
  const handleResumeMission = async () => {
    const mission = viewingMission ?? currentMission;
    if (!mission || !["interrupted", "blocked", "failed"].includes(mission.status)) return;
    try {
      setMissionLoading(true);
      const resumed = await resumeMission(mission.id);
      setCurrentMission(resumed);
      setViewingMission(resumed);
      setViewingMissionId(resumed.id);
      // Show basic history immediately
      const basicItems = missionHistoryToItems(resumed);
      setItems(basicItems);
      adjustVisibleItemsLimit(basicItems);
      updateMissionItems(resumed.id, basicItems);
      refreshRecentMissions();
      toast.success(
        mission.status === "blocked"
          ? "Continuing mission"
          : mission.status === "failed"
            ? "Retrying mission"
            : "Mission resumed"
      );
      // Load full events in background (including tool calls)
      loadHistoryEvents(resumed.id)
        .then((events) => {
          let fullItems = eventsToItems(events, resumed);
          if (!fullItems.some((item) => item.kind === "assistant")) {
            const historyHasAssistant = resumed.history.some(
              (entry) => entry.role === "assistant"
            );
            if (historyHasAssistant) {
              fullItems = missionHistoryToItems(resumed);
            }
          }
          setItems(fullItems);
          adjustVisibleItemsLimit(fullItems);
          updateMissionItems(resumed.id, fullItems);
          // Also check events for desktop sessions
          applyDesktopSessionFromEvents(events);
        })
        .catch(() => {}); // Keep basic history on failure
    } catch (err) {
      console.error("Failed to resume mission:", err);
      toast.error("Failed to resume mission");
    } finally {
      setMissionLoading(false);
    }
  };

  // Debouncing for thinking updates to reduce re-renders during streaming
  const pendingThinkingRef = useRef<{
    content: string;
    done: boolean;
    id: string;
    startTime: number;
  } | null>(null);
  const thinkingFlushTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const thinkingIdCounterRef = useRef(0);

  const pendingStreamRef = useRef<{
    content: string;
    startTime: number;
  } | null>(null);
  const streamFlushTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Auto-reconnecting stream with exponential backoff
  useEffect(() => {
    let cleanup: (() => void) | null = null;
    let reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
    let reconnectAttempts = 0;
    let mounted = true;
    const maxReconnectDelay = 30000;
    const baseDelay = 1000;

    // Fetch initial progress for refresh resilience
    getProgress()
      .then((p) => {
        if (mounted && p.total_subtasks > 0) {
          const currentId = currentMissionRef.current?.id;
          if (currentId) {
            setProgressByMission((prev) => ({
              ...prev,
              [currentId]: {
                total: p.total_subtasks,
                completed: p.completed_subtasks,
                current: p.current_subtask,
                depth: p.current_depth,
              },
            }));
          }
        }
      })
      .catch(() => {}); // Ignore errors

    const handleEvent = (event: { type: string; data: unknown }) => {
      const data: unknown = event.data;

      // Filter events by mission_id - only show events for the mission we're viewing
      const viewingId = viewingMissionIdRef.current;
      const eventMissionId =
        isRecord(data) && data["mission_id"]
          ? String(data["mission_id"])
          : null;
      const currentMissionId = currentMissionRef.current?.id;
      streamLog("debug", "received", {
        type: event.type,
        eventMissionId,
        viewingId,
        currentMissionId,
      });

      // If we're viewing a specific mission, filter events strictly
      if (viewingId) {
        let filterReason: string | null = null;
        // Event has a mission_id - must match viewing mission
        if (eventMissionId) {
          if (eventMissionId !== viewingId) {
            // Event is from a different mission - only allow status events
            if (event.type !== "status") {
              filterReason = "event from different mission";
            }
          }
        } else {
          // Event has NO mission_id (from main session)
          // Only show if we're viewing the current/main mission OR if currentMission
          // hasn't been loaded yet (to handle race condition during initial load)
          if (currentMissionId && viewingId !== currentMissionId) {
            // We're viewing a parallel mission, skip main session events
            if (event.type !== "status") {
              filterReason = "event has no mission_id for parallel mission";
            }
          }
        }
        if (filterReason) {
          streamLog("debug", "filtered", {
            type: event.type,
            eventMissionId,
            viewingId,
            currentMissionId,
            reason: filterReason,
          });
          return;
        }
      }

      if (event.type === "status" && isRecord(data)) {
        const wasReconnecting = reconnectAttempts > 0;
        reconnectAttempts = 0;

        // Update connection state to connected
        setConnectionState("connected");
        setReconnectAttempt(0);

        // If we just reconnected, refresh the viewed mission's history to catch missed events
        if (wasReconnecting && viewingId) {
          reloadMissionHistory(viewingId);
        }

        const st = data["state"];
        const newState =
          typeof st === "string" ? (st as ControlRunState) : "idle";
        const q = data["queue_len"];

        // Status filtering: only apply UI side-effects if it matches the mission we're viewing
        const statusMissionId =
          typeof data["mission_id"] === "string" ? data["mission_id"] : null;
        const effectiveMissionId =
          statusMissionId ?? runStateMissionIdRef.current ?? null;
        let shouldApplyStatus = true;

        if (effectiveMissionId) {
          shouldApplyStatus = effectiveMissionId === viewingId;
        } else {
          // No mission id available - only apply if viewing main mission or none selected
          shouldApplyStatus = !viewingId || viewingId === currentMissionId || !currentMissionId;
        }

        const nextQueueLen = typeof q === "number" ? q : 0;
        setQueueLen(nextQueueLen);
        setRunStateMissionId(effectiveMissionId);

        if (shouldApplyStatus && effectiveMissionId) {
          const prevQueueLen = lastQueueLenRef.current;
          lastQueueLenRef.current = nextQueueLen;
          if (prevQueueLen !== null && nextQueueLen < prevQueueLen) {
            syncQueueForMission(effectiveMissionId);
          }
        }

        // Clear progress and auto-close desktop stream when idle for the active mission
        if (newState === "idle" && effectiveMissionId) {
          setProgressByMission((prev) => {
            if (!prev[effectiveMissionId]) return prev;
            const { [effectiveMissionId]: _removed, ...rest } = prev;
            return rest;
          });
          if (shouldApplyStatus) {
            // Auto-close desktop stream when agent finishes, unless a session is still running.
            if (!hasRunningDesktopSessionForMission(effectiveMissionId) && !hasDesktopSessionRef.current) {
              setShowDesktopStream(false);
            }
          }
        }

        // If we reconnected and agent is already running, add a visual indicator
        setRunState((prevState) => {
          if (shouldApplyStatus && newState === "running" && prevState === "idle") {
            setItems((prevItems) => {
              const hasActiveThinking = prevItems.some(
                (it) =>
                  ((it.kind === "thinking" || it.kind === "stream") &&
                    !it.done) ||
                  it.kind === "phase"
              );
              // If there's no active streaming item, the user is seeing stale state
              // The "Agent is working..." indicator will show via the render logic
              return prevItems;
            });
          }
          return newState;
        });
        return;
      }

      if (event.type === "user_message" && isRecord(data)) {
        const msgId = String(data["id"] ?? Date.now());
        const msgContent = String(data["content"] ?? "");
        const hasQueuedFlag = Object.prototype.hasOwnProperty.call(data, "queued");
        const queued = data["queued"] === true;
        setItems((prev) => {
          // Check if already added with this ID - if so, mark as not queued (being processed)
          const existingIndex = prev.findIndex((item) => item.id === msgId);
          if (existingIndex !== -1) {
            const existing = prev[existingIndex];
            if (existing.kind === "user") {
              const nextQueued = hasQueuedFlag ? queued : existing.queued;
              if (existing.queued !== nextQueued) {
                const updated = [...prev];
                updated[existingIndex] = { ...existing, queued: nextQueued };
                return updated;
              }
            }
            return prev;
          }

          // Check if there's a pending temp message with matching content (SSE arrived before API response)
          // We verify content to avoid mismatching with messages from other sessions/devices
          const tempIndex = prev.findIndex(
            (item) =>
              item.kind === "user" &&
              item.id.startsWith("temp-") &&
              item.content === msgContent
          );

          if (tempIndex !== -1) {
            // Replace temp ID with server ID, mark as not queued (being processed)
            const updated = [...prev];
            const tempItem = updated[tempIndex];
            if (tempItem.kind === "user") {
              updated[tempIndex] = {
                ...tempItem,
                id: msgId,
                queued: hasQueuedFlag ? queued : tempItem.queued,
              };
            }
            return updated;
          }

          // Check if there's an existing user message with the same content but a non-server ID
          // (e.g., history-* ID from missionHistoryToItems that replaced the UUID-based item).
          // Search from the end to match the most recent message with this content,
          // and only match if the ID is not already a server-assigned UUID.
          const contentIndex = [...prev].reverse().findIndex(
            (item) =>
              item.kind === "user" &&
              item.content === msgContent &&
              (item.id.startsWith("history-") || item.id.startsWith("temp-"))
          );
          if (contentIndex !== -1) {
            // Convert reversed index back to forward index
            const actualIndex = prev.length - 1 - contentIndex;
            const existing = prev[actualIndex];
            if (existing.kind === "user") {
              const updated = [...prev];
              updated[actualIndex] = {
                ...existing,
                id: msgId,
                queued: hasQueuedFlag ? queued : existing.queued,
              };
              return updated;
            }
          }

          // No matching message found at all, add new (message came from another client/session)
          return [
            ...prev,
            {
              kind: "user",
              id: msgId,
              content: msgContent,
              timestamp: Date.now(),
              queued,
            },
          ];
        });
        return;
      }

      if (event.type === "assistant_message" && isRecord(data)) {
        const now = Date.now();
        // Parse shared_files if present
        let sharedFiles: SharedFile[] | undefined;
        if (Array.isArray(data["shared_files"])) {
          sharedFiles = (data["shared_files"] as unknown[]).filter(isRecord).map((f) => ({
            name: String(f["name"] ?? "file"),
            url: String(f["url"] ?? ""),
            content_type: String(f["content_type"] ?? "application/octet-stream"),
            size_bytes: typeof f["size_bytes"] === "number" ? f["size_bytes"] : undefined,
            kind: (f["kind"] as SharedFile["kind"]) ?? "other",
          }));
        }

        const resumable = data["resumable"] === true;
        // Use strict equality to match eventsToItems behavior:
        // undefined means no explicit status, only false means actual failure
        const isFailure = data["success"] === false;
        const incomingId = String(data["id"] ?? Date.now());

        // Finalize any pending thinking session when an assistant message arrives.
        if (thinkingFlushTimeoutRef.current) {
          clearTimeout(thinkingFlushTimeoutRef.current);
          thinkingFlushTimeoutRef.current = null;
        }
        pendingThinkingRef.current = null;
        if (streamFlushTimeoutRef.current) {
          clearTimeout(streamFlushTimeoutRef.current);
          streamFlushTimeoutRef.current = null;
        }
        pendingStreamRef.current = null;

        setItems((prev) => {
          // Mark any in-progress thinking as done instead of dropping it,
          // so the Thinking panel can show a scrollable history.
          let filtered = prev.map((it) => {
            if ((it.kind === "thinking" || it.kind === "stream") && !it.done) {
              return { ...it, done: true, endTime: now };
            }
            return it;
          });

          // When mission fails, mark all pending tool calls as failed
          // This ensures subagent headers don't stay stuck showing "Running for X"
          if (isFailure) {
            const errorMessage = String(data["content"] ?? "Mission failed");
            filtered = filtered.map((it) => {
              if (it.kind === "tool" && it.result === undefined) {
                return {
                  ...it,
                  result: { error: errorMessage, status: "failed" },
                  endTime: Date.now(),
                };
              }
              return it;
            });
          }

          const existingIdx = filtered.findIndex(
            (item) => item.kind === "assistant" && item.id === incomingId
          );
          if (existingIdx !== -1) {
            const updated = [...filtered];
            const existing = updated[existingIdx] as Extract<
              ChatItem,
              { kind: "assistant" }
            >;
            updated[existingIdx] = {
              ...existing,
              content: String(data["content"] ?? existing.content),
              success: !isFailure,
              ...parseCostMetadata(data, {
                costCents: existing.costCents,
                costSource: existing.costSource,
              }),
              model: data["model"] ? String(data["model"]) : existing.model ?? null,
              timestamp: now,
              sharedFiles: sharedFiles ?? existing.sharedFiles,
              resumable,
            };
            return updated;
          }

          const newItem: ChatItem = {
            kind: "assistant",
            id: incomingId,
            content: String(data["content"] ?? ""),
            success: !isFailure,
            ...parseCostMetadata(data),
            model: data["model"] ? String(data["model"]) : null,
            timestamp: now,
            sharedFiles,
            resumable,
          };

          const firstQueuedIdx = filtered.findIndex(
            (item) => item.kind === "user" && item.queued
          );
          if (firstQueuedIdx === -1) {
            return [...filtered, newItem];
          }
          const updated = [...filtered];
          updated.splice(firstQueuedIdx, 0, newItem);
          return updated;
        });

        // Reset stream phase to idle when agent finishes responding
        // (Agent has completed processing and is now waiting for user input)
        setStreamDiagnostics((prev) => ({
          ...prev,
          phase: "idle",
        }));
        return;
      }

      if (event.type === "thinking" && isRecord(data)) {
        const content = String(data["content"] ?? "");
        const done = Boolean(data["done"]);
        const now = Date.now();

        // Debounced thinking updates to reduce re-renders during streaming
        const flushThinking = () => {
          const pending = pendingThinkingRef.current;
          if (!pending) return;

          setItems((prev) => {
            // Remove phase items when thinking starts
            const filtered = prev.filter((it) => it.kind !== "phase");
            const existingIdx = filtered.findIndex(
              (it) => it.kind === "thinking" && !it.done
            );
            if (existingIdx >= 0) {
              const updated = [...filtered];
              const existing = updated[existingIdx] as Extract<
                ChatItem,
                { kind: "thinking" }
              >;

              // Update existing item in place with buffered content
              if (pending.done || !pending.content || existing.id === pending.id) {
                updated[existingIdx] = {
                  ...existing,
                  content: pending.content || existing.content,
                  done: pending.done,
                  endTime: pending.done ? now : existing.endTime,
                };
                if (pending.done) {
                  pendingThinkingRef.current = null;
                }
                return updated;
              }

              // New thought - mark existing as done and create new
              updated[existingIdx] = {
                ...existing,
                done: true,
                endTime: now,
              };
              if (pending.done) {
                pendingThinkingRef.current = null;
              }
              return [
                ...updated,
                {
                  kind: "thinking" as const,
                  id: pending.id,
                  content: pending.content,
                  done: pending.done,
                  startTime: pending.startTime,
                  endTime: pending.done ? now : undefined,
                },
              ];
            } else {
              if (pending.done) {
                pendingThinkingRef.current = null;
              }
              return [
                ...filtered,
                {
                  kind: "thinking" as const,
                  id: pending.id,
                  content: pending.content,
                  done: pending.done,
                  startTime: pending.startTime,
                  endTime: pending.done ? now : undefined,
                },
              ];
            }
          });
        };

        // Get or create stable ID for current thinking session
        const existingPending = pendingThinkingRef.current;
        const existingContent = existingPending?.content ?? "";
        const isContinuation =
          !content ||
          !existingContent ||
          content.startsWith(existingContent) ||
          existingContent.startsWith(content);
        const shouldStartNew = Boolean(existingPending && !isContinuation && existingContent.trim());

        if (shouldStartNew) {
          // Finalize the previous thought before starting a new one.
          pendingThinkingRef.current = {
            content: existingContent,
            done: true,
            id: existingPending?.id ?? `thinking-${thinkingIdCounterRef.current++}`,
            startTime: existingPending?.startTime ?? now,
          };
          flushThinking();
        }

        const thinkingId = shouldStartNew
          ? `thinking-${thinkingIdCounterRef.current++}`
          : existingPending?.id ?? `thinking-${thinkingIdCounterRef.current++}`;
        const startTime = shouldStartNew ? now : existingPending?.startTime ?? now;

        // Buffer the content update
        pendingThinkingRef.current = {
          content: content || existingPending?.content || "",
          done,
          id: thinkingId,
          startTime,
        };

        // Clear any pending flush timeout
        if (thinkingFlushTimeoutRef.current) {
          clearTimeout(thinkingFlushTimeoutRef.current);
          thinkingFlushTimeoutRef.current = null;
        }

        // Flush immediately if done, otherwise debounce (100ms)
        if (done) {
          flushThinking();
        } else {
          thinkingFlushTimeoutRef.current = setTimeout(flushThinking, 100);
        }
        return;
      }

      if (event.type === "text_delta" && isRecord(data)) {
        const content = String(data["content"] ?? "");
        const now = Date.now();
        if (!content.trim()) return;

        // Debounced stream updates to reduce re-renders during rapid delta streaming.
        const flushStream = () => {
          const pending = pendingStreamRef.current;
          if (!pending) return;

          setItems((prev) => {
            // Remove phase items when streaming starts
            const filtered = prev.filter((it) => it.kind !== "phase");
            const streamId = "text_delta_latest";
            const existingIdx = filtered.findIndex(
              (it) => it.kind === "stream" && it.id === streamId
            );
            if (existingIdx >= 0) {
              const updated = [...filtered];
              const existing = updated[existingIdx] as Extract<
                ChatItem,
                { kind: "stream" }
              >;
              const existingContent = existing.content ?? "";
              const isContinuation =
                !pending.content ||
                !existingContent ||
                pending.content.startsWith(existingContent) ||
                existingContent.startsWith(pending.content);
              updated[existingIdx] = {
                ...existing,
                content: pending.content || existing.content,
                done: false,
                startTime:
                  isContinuation && !existing.done
                    ? existing.startTime
                    : pending.startTime,
                endTime: undefined,
              };
              return updated;
            }

            // No active stream item yet - create one.
            return [
              ...filtered,
              {
                kind: "stream" as const,
                id: "text_delta_latest",
                content: pending.content,
                done: false,
                startTime: pending.startTime,
                endTime: undefined,
              },
            ];
          });
        };

        const existingPending = pendingStreamRef.current;
        const existingContent = existingPending?.content ?? "";
        const isContinuation =
          !content ||
          !existingContent ||
          content.startsWith(existingContent) ||
          existingContent.startsWith(content);

        pendingStreamRef.current = {
          content: content || existingPending?.content || "",
          startTime: isContinuation ? existingPending?.startTime ?? now : now,
        };

        if (streamFlushTimeoutRef.current) {
          clearTimeout(streamFlushTimeoutRef.current);
          streamFlushTimeoutRef.current = null;
        }
        streamFlushTimeoutRef.current = setTimeout(flushStream, 100);
        return;
      }

      if (event.type === "tool_call" && isRecord(data)) {
        const name = String(data["name"] ?? "");
        const isUiTool = name.startsWith("ui_") || name === "question" || name === "AskUserQuestion";
        const toolCallId = String(data["tool_call_id"] ?? "");

        setItems((prev) => {
          const existingIdx = prev.findIndex(
            (item) => item.kind === "tool" && item.toolCallId === toolCallId
          );
          if (existingIdx !== -1) {
            return prev;
          }

          const toolItem: ChatItem = {
            kind: "tool",
            id: `tool-${toolCallId || Date.now()}`,
            toolCallId,
            name,
            args: data["args"],
            isUiTool,
            startTime: Date.now(),
          };

          // Important: keep queued user messages at the end of the timeline.
          // If we append tool calls after a queued message, the UI can appear to "lose"
          // the assistant reply (it may be inserted before the queued message and then
          // scrolled out of view under a long tail of tools).
          const firstQueuedIdx = prev.findIndex(
            (item) => item.kind === "user" && item.queued === true
          );
          if (firstQueuedIdx === -1) {
            return [...prev, toolItem];
          }
          const updated = [...prev];
          updated.splice(firstQueuedIdx, 0, toolItem);
          return updated;
        });

        // Detect desktop_start_session from ToolCall (Claude Code/Amp don't emit ToolResult for MCP tools)
        const isDesktopStart =
          name === "desktop_start_session" ||
          name === "desktop_desktop_start_session" ||
          name === "mcp__desktop__desktop_start_session";
        if (isDesktopStart) {
          setHasDesktopSession(true);
          setShowDesktopStream(true);
          expectingDesktopSessionRef.current = true;
          // Start rapid polling (every 2s) to pick up the session once the backend attributes it
          if (desktopRapidPollRef.current) clearInterval(desktopRapidPollRef.current);
          desktopRapidPollRef.current = setInterval(() => {
            refreshDesktopSessions();
          }, 2000);
          // Stop rapid polling after 30s
          setTimeout(() => {
            if (desktopRapidPollRef.current) {
              clearInterval(desktopRapidPollRef.current);
              desktopRapidPollRef.current = null;
            }
            expectingDesktopSessionRef.current = false;
          }, 30000);
        }

        return;
      }

      if (event.type === "tool_result" && isRecord(data)) {
        const toolCallId = String(data["tool_call_id"] ?? "");
        const endTime = Date.now();

        // Extract display ID from desktop_start_session tool result
        // Get tool name from the event data (preferred) or fall back to stored tool item
        const eventToolName = typeof data["name"] === "string" ? data["name"] : null;

        // Check for desktop_start_session right away using event data
        // This handles the case where tool_call events might be filtered or missed
        if (eventToolName === "desktop_start_session" || eventToolName === "desktop_desktop_start_session" || eventToolName === "mcp__desktop__desktop_start_session") {
          const display = extractDesktopDisplay(data["result"] ?? data);
          if (display) {
            setDesktopDisplayId(display);
            setHasDesktopSession(true);
            // Auto-open desktop stream when session starts
            setShowDesktopStream(true);
          }
        }
        // Handle desktop session close
        if (eventToolName === "desktop_close_session" || eventToolName === "desktop_desktop_close_session" || eventToolName === "mcp__desktop__desktop_close_session") {
          setHasDesktopSession(false);
          setShowDesktopStream(false);
        }

        // If eventToolName wasn't available, check stored items for desktop session tools
        // Use itemsRef for synchronous read to avoid side effects in state updaters
        if (!eventToolName) {
          const toolItem = itemsRef.current.find(
            (it) => it.kind === "tool" && it.toolCallId === toolCallId
          );
          if (toolItem && toolItem.kind === "tool") {
            const toolName = toolItem.name;
            // Check for desktop_start_session (with or without desktop_ prefix from MCP)
            if (toolName === "desktop_start_session" || toolName === "desktop_desktop_start_session" || toolName === "mcp__desktop__desktop_start_session") {
              const display = extractDesktopDisplay(data["result"] ?? data);
              if (display) {
                setDesktopDisplayId(display);
                setHasDesktopSession(true);
                setShowDesktopStream(true);
              }
            }
            // Check for desktop_close_session
            if (toolName === "desktop_close_session" || toolName === "desktop_desktop_close_session" || toolName === "mcp__desktop__desktop_close_session") {
              setHasDesktopSession(false);
              setShowDesktopStream(false);
            }
          }
        }

        setItems((prev) =>
          prev.map((it) =>
            it.kind === "tool" && it.toolCallId === toolCallId
              ? { ...it, result: data["result"], endTime }
              : it
          )
        );
        return;
      }

      if (event.type === "agent_phase" && isRecord(data)) {
        const phase = String(data["phase"] ?? "");
        const detail = data["detail"] ? String(data["detail"]) : null;
        const agent = data["agent"] ? String(data["agent"]) : null;

        // Update or add phase item (we only keep one active phase at a time)
        setItems((prev) => {
          // Remove any existing phase items
          const filtered = prev.filter((it) => it.kind !== "phase");
          return [
            ...filtered,
            {
              kind: "phase" as const,
              id: `phase-${Date.now()}`,
              phase,
              detail,
              agent,
            },
          ];
        });
        return;
      }

      if (event.type === "error") {
        const msg =
          (isRecord(data) && data["message"]
            ? String(data["message"])
            : null) ?? "An error occurred.";
        const resumable = isRecord(data) && data["resumable"] === true;
        const missionId = isRecord(data) && typeof data["mission_id"] === "string"
          ? data["mission_id"]
          : undefined;
        streamLog("error", "error event", {
          message: msg,
          missionId,
          resumable,
        });

        if (
          msg.includes("Stream connection failed") ||
          msg.includes("Stream ended")
        ) {
          scheduleReconnect();
        } else {
          setItems((prev) => [
            ...prev,
            { kind: "system", id: `err-${Date.now()}`, content: msg, timestamp: Date.now(), resumable, missionId },
          ]);
          toast.error(msg);
        }
      }

      // Handle mission status changes
      if (event.type === "mission_status_changed" && isRecord(data)) {
        const newStatus = String(data["status"] ?? "");
        const missionId = typeof data["mission_id"] === "string" ? data["mission_id"] : undefined;

        // Always update mission status in state when it changes
        if (missionId) {
          if (currentMissionRef.current?.id === missionId) {
            setCurrentMission((prev) =>
              prev ? { ...prev, status: newStatus as MissionStatus } : prev
            );
          }
          if (viewingMissionRef.current?.id === missionId) {
            setViewingMission((prev) =>
              prev ? { ...prev, status: newStatus as MissionStatus } : prev
            );
          }
        }

        // When mission is no longer active, mark all pending tool calls as cancelled
        if (newStatus !== "active") {
          const now = Date.now();
          setItems((prev) =>
            prev.map((item) => {
              if ((item.kind === "thinking" || item.kind === "stream") && !item.done) {
                return { ...item, done: true, endTime: now };
              }
              if (item.kind === "tool" && item.result === undefined) {
                return {
                  ...item,
                  result: { status: "cancelled", reason: `Mission ${newStatus}` },
                  endTime: now,
                };
              }
              return item;
            })
          );
          if (thinkingFlushTimeoutRef.current) {
            clearTimeout(thinkingFlushTimeoutRef.current);
            thinkingFlushTimeoutRef.current = null;
          }
          pendingThinkingRef.current = null;
          if (streamFlushTimeoutRef.current) {
            clearTimeout(streamFlushTimeoutRef.current);
            streamFlushTimeoutRef.current = null;
          }
          pendingStreamRef.current = null;

          // Reset stream phase to idle when mission completes
          // (The SSE connection stays open for the control session, but the mission is done)
          setStreamDiagnostics((prev) => ({
            ...prev,
            phase: "idle",
          }));
        }
      }

      // Handle progress updates
      if (event.type === "progress" && isRecord(data)) {
        const progressMissionId =
          typeof data["mission_id"] === "string"
            ? data["mission_id"]
            : currentMissionRef.current?.id ?? null;
        if (progressMissionId) {
          setProgressByMission((prev) => ({
            ...prev,
            [progressMissionId]: {
              total: Number(data["total_subtasks"] ?? 0),
              completed: Number(data["completed_subtasks"] ?? 0),
              current: data["current_subtask"] as string | null,
              depth: Number(data["depth"] ?? 0),
            },
          }));
        }
      }
    };

    const scheduleReconnect = () => {
      if (!mounted) return;
      const delay = Math.min(
        baseDelay * Math.pow(2, reconnectAttempts),
        maxReconnectDelay
      );
      reconnectAttempts++;
      streamLog("warn", "reconnect scheduled", {
        attempt: reconnectAttempts,
        delayMs: delay,
      });
      // Update connection state to show reconnecting indicator
      setConnectionState("reconnecting");
      setReconnectAttempt(reconnectAttempts);
      reconnectTimeout = setTimeout(() => {
        if (mounted) connect();
      }, delay);
    };

    const connect = () => {
      cleanup?.();
      streamLog("info", "connecting stream");
      cleanup = streamControl(handleEvent, handleStreamDiagnostics);
    };

    connect();
    streamCleanupRef.current = cleanup;

    return () => {
      mounted = false;
      if (reconnectTimeout) clearTimeout(reconnectTimeout);
      cleanup?.();
      streamCleanupRef.current = null;
      // Clean up thinking debounce timeout
      if (thinkingFlushTimeoutRef.current) {
        clearTimeout(thinkingFlushTimeoutRef.current);
        thinkingFlushTimeoutRef.current = null;
      }
      if (streamFlushTimeoutRef.current) {
        clearTimeout(streamFlushTimeoutRef.current);
        streamFlushTimeoutRef.current = null;
      }
      pendingStreamRef.current = null;
    };
  }, []);

  const status = useMemo(() => statusLabel(viewingRunState), [viewingRunState]);
  const StatusIcon = status.Icon;

  const streamHints = useMemo(() => {
    const hints: string[] = [];
    const ct = streamDiagnostics.contentType;
    if (ct && !ct.toLowerCase().includes("text/event-stream")) {
      hints.push(`Content-Type is "${ct}" (expected text/event-stream).`);
    }
    const ce = streamDiagnostics.contentEncoding;
    if (ce && ce !== "identity") {
      hints.push(`Content-Encoding is "${ce}". Disable gzip for SSE.`);
    }
    if (
      streamDiagnostics.phase === "open" ||
      streamDiagnostics.phase === "streaming"
    ) {
      const lastChunkAge = streamDiagnostics.lastChunkAt
        ? Date.now() - streamDiagnostics.lastChunkAt
        : null;
      if (lastChunkAge !== null && lastChunkAge > 30000) {
        hints.push("No SSE chunks for >30s. Likely proxy buffering or connection drops.");
      }
    }
    if (typeof streamDiagnostics.status === "number" && streamDiagnostics.status >= 400) {
      hints.push(`Stream request returned HTTP ${streamDiagnostics.status}.`);
    }
    return hints;
  }, [streamDiagnostics, diagTick]);

  const handleCopyDiagnostics = useCallback(async () => {
    const mission = viewingMission ?? currentMission;
    const payload = {
      captured_at: new Date().toISOString(),
      mission: mission ? {
        id: mission.id,
        status: mission.status,
        title: mission.title,
        workspace_id: mission.workspace_id,
        workspace_name: mission.workspace_name,
      } : null,
      stream: {
        phase: streamDiagnostics.phase,
        status: streamDiagnostics.status,
        bytes: streamDiagnostics.bytes,
        last_event: streamDiagnostics.lastEventAt,
        last_error: streamDiagnostics.lastError,
      },
      connection_state: connectionState,
      reconnect_attempt: reconnectAttempt,
    };
    try {
      await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
      toast.success("Copied debug info");
    } catch {
      toast.error("Failed to copy");
    }
  }, [connectionState, reconnectAttempt, streamDiagnostics, viewingMission, currentMission]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const content = input.trim();
    if (!content) return;

    const targetMissionId = viewingMissionIdRef.current;

    // Always sync with backend before sending to prevent mission routing bugs.
    // The backend's current_mission can get out of sync (e.g., from another tab or auto-creation).
    if (targetMissionId) {
      try {
        console.debug("[control] syncing mission before send", { targetMissionId });
        const mission = await loadMission(targetMissionId);
        if (!mission) {
          toast.error("Mission not found");
          return;
        }
        setCurrentMission(mission);
        setViewingMission(mission);
        setViewingMissionId(mission.id);
        // Only update items if history content has actually changed
        if (hasHistoryChanged(items, mission.history)) {
          setItems(missionHistoryToItems(mission));
        }
        applyDesktopSessionState(mission);
      } catch (err) {
        const errMsg = err instanceof Error ? err.message : String(err);
        console.error("Failed to sync mission before sending:", err);
        toast.error(`Failed to sync mission: ${errMsg}. Check API connection in Settings.`);
        return;
      }
    }

    setInput("");
    setDraftInput("");

    // Generate temp ID and add message optimistically BEFORE the API call
    // This ensures messages appear in send order, not response order
    const tempId = `temp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const timestamp = Date.now();

    // Message is queued only if agent is currently busy AND there are existing user messages
    // The first message in a conversation should never be shown as queued
    const hasExistingUserMessages = items.some((item) => item.kind === "user");
    const willBeQueued = isBusy && hasExistingUserMessages;

    setItems((prev) => [
      ...prev,
      {
        kind: "user" as const,
        id: tempId,
        content,
        timestamp,
        queued: willBeQueued,
      },
    ]);

    try {
      const { id, queued } = await postControlMessage(content);

      // Replace temp ID with server-assigned ID and update queued status
      // This allows SSE handler to correctly deduplicate
      // The first message should never be shown as queued
      setItems((prev) => {
        // Check if SSE already added this message (race condition where SSE arrives before API response)
        // If so, just remove the temp message to avoid duplicates
        const sseAlreadyAdded = prev.some((item) => item.id === id);
        if (sseAlreadyAdded) {
          return prev.filter((item) => item.id !== tempId);
        }

        const otherUserMessages = prev.filter((item) => item.kind === "user" && item.id !== tempId);
        const isFirstMessage = otherUserMessages.length === 0;
        const effectiveQueued = isFirstMessage ? false : queued;
        return prev.map((item) =>
          item.id === tempId ? { ...item, id, queued: effectiveQueued } : item
        );
      });
    } catch (err) {
      console.error(err);
      // Remove the optimistic message on error
      setItems((prev) => prev.filter((item) => item.id !== tempId));
      toast.error("Failed to send message");
    }
  };

  // Handler for EnhancedInput that takes a payload with content and optional agent
  const handleEnhancedSubmit = useCallback(async (payload: SubmitPayload) => {
    const { content, agent } = payload;
    if (!content.trim()) return;

    // Guard against double-submission (e.g., double-click, React StrictMode)
    if (submittingRef.current) {
      console.debug("[control] ignoring duplicate submission");
      return;
    }
    submittingRef.current = true;

    const targetMissionId = viewingMissionIdRef.current;

    // Sync mission state before sending (backend needs current_mission set correctly)
    if (targetMissionId) {
      try {
        let mission = await loadMission(targetMissionId);

        if (!mission) {
          toast.error("Mission not found");
          submittingRef.current = false;
          return;
        }

        // If the mission is in a resumable state (failed/interrupted/blocked),
        // resume it first to update the status before sending the message.
        // Use skipMessage to avoid the auto-generated "MISSION RESUMED" message
        // since the user is about to send their own custom message.
        if (["failed", "interrupted", "blocked"].includes(mission.status)) {
          mission = await resumeMission(mission.id, { skipMessage: true });
        }

        setCurrentMission(mission);
        setViewingMission(mission);
        setViewingMissionId(mission.id);
        // Don't sync items from persisted history here - the local items state
        // is the source of truth and may contain SSE-delivered content that
        // hasn't been persisted yet. Replacing items would cause messages to disappear.
        applyDesktopSessionState(mission);
      } catch (err) {
        const errMsg = err instanceof Error ? err.message : String(err);
        console.error("Failed to sync mission before sending:", err);
        toast.error(`Failed to sync mission: ${errMsg}. Check API connection in Settings.`);
        submittingRef.current = false;
        return;
      }
    }

    setInput("");
    setDraftInput("");

    const tempId = `temp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const timestamp = Date.now();
    const hasExistingUserMessages = items.some((item) => item.kind === "user");
    const willBeQueued = isBusy && hasExistingUserMessages;

    // Use raw content for optimistic message (not prefixed with agent)
    // This ensures content matches what SSE echoes back, preventing duplicate messages
    // when SSE arrives before API response and needs to dedupe by content
    setItems((prev) => [
      ...prev,
      {
        kind: "user" as const,
        id: tempId,
        content,
        timestamp,
        queued: willBeQueued,
      },
    ]);

    try {
      // Send message with mission_id - backend handles routing (main vs parallel)
      const { id, queued } = await postControlMessage(content, {
        agent: agent || undefined,
        mission_id: targetMissionId || undefined,
      });
      setItems((prev) => {
        // Check if SSE already added this message (race condition where SSE arrives before API response)
        // If so, just remove the temp message to avoid duplicates
        const sseAlreadyAdded = prev.some((item) => item.id === id);
        if (sseAlreadyAdded) {
          return prev.filter((item) => item.id !== tempId);
        }

        const otherUserMessages = prev.filter((item) => item.kind === "user" && item.id !== tempId);
        const isFirstMessage = otherUserMessages.length === 0;
        const effectiveQueued = isFirstMessage ? false : queued;
        return prev.map((item) =>
          item.id === tempId ? { ...item, id, queued: effectiveQueued } : item
        );
      });
    } catch (err) {
      console.error(err);
      setItems((prev) => prev.filter((item) => item.id !== tempId));
      toast.error("Failed to send message");
    } finally {
      submittingRef.current = false;
    }
  }, [items, isBusy, applyDesktopSessionState, missionHistoryToItems]);

  const handleStop = async () => {
    const targetId = viewingMissionIdRef.current;
    if (targetId) {
      await handleCancelMission(targetId);
      return;
    }
    try {
      await cancelControl();
      toast.success("Cancelled");
    } catch (err) {
      console.error(err);
      toast.error("Failed to cancel");
    }
  };

  const syncQueueForMission = useCallback(async (missionId: string) => {
    if (!missionId || syncingQueueRef.current) return;
    syncingQueueRef.current = true;
    try {
      const queuedMessages = await getQueue();
      const queuedForMission = queuedMessages.filter((qm) => qm.mission_id === missionId);
      const queuedIds = new Set(queuedForMission.map((qm) => qm.id));

      setItems((prev) =>
        prev.map((item) => {
          if (item.kind !== "user") return item;
          if (item.id.startsWith("temp-")) return item;
          const shouldBeQueued = queuedIds.has(item.id);
          if (item.queued === shouldBeQueued) return item;
          return { ...item, queued: shouldBeQueued };
        })
      );
    } catch (err) {
      console.warn("[control] failed to sync queue", err);
    } finally {
      syncingQueueRef.current = false;
    }
  }, []);

  // Reload full mission history from API (events + queue). Used for visibility
  // change, periodic sync, and SSE reconnect catch-up.
  const reloadMissionHistory = useCallback(
    async (missionId: string) => {
      try {
        const [mission, events, queuedMessages] = await Promise.all([
          getMission(missionId),
          loadHistoryEvents(missionId).catch(() => null),
          getQueue().catch(() => []),
        ]);
        // Race guard: only apply if we're still viewing this mission
        if (viewingMissionIdRef.current !== missionId) return;

        let historyItems = events
          ? eventsToItems(events, mission)
          : missionHistoryToItems(mission);
        if (events && !historyItems.some((item) => item.kind === "assistant")) {
          const historyHasAssistant = mission.history.some(
            (entry) => entry.role === "assistant"
          );
          if (historyHasAssistant) {
            historyItems = missionHistoryToItems(mission);
          }
        }

        // Merge queued messages that belong to this mission
        const missionQueuedMessages = queuedMessages.filter(
          (qm) => qm.mission_id === missionId
        );
        if (missionQueuedMessages.length > 0) {
          const queuedIds = new Set(missionQueuedMessages.map((qm) => qm.id));
          historyItems = historyItems.map((item) =>
            item.kind === "user" && queuedIds.has(item.id)
              ? { ...item, queued: true }
              : item
          );
          const existingIds = new Set(historyItems.map((item) => item.id));
          const newQueuedItems: ChatItem[] = missionQueuedMessages
            .filter((qm) => !existingIds.has(qm.id))
            .map((qm) => ({
              kind: "user" as const,
              id: qm.id,
              content: qm.content,
              timestamp: Date.now(),
              agent: qm.agent ?? undefined,
              queued: true,
            }));
          historyItems = [...historyItems, ...newQueuedItems];
        }

        setItems(historyItems);
        adjustVisibleItemsLimit(historyItems);
        updateMissionItems(missionId, historyItems);
        if (events) {
          applyDesktopSessionFromEvents(events);
        }
      } catch (err) {
        console.warn("[control] reloadMissionHistory failed", err);
      }
    },
    [
      loadHistoryEvents,
      eventsToItems,
      missionHistoryToItems,
      adjustVisibleItemsLimit,
      updateMissionItems,
      applyDesktopSessionFromEvents,
    ]
  );

  // Reload full history when the tab regains visibility to catch missed SSE events
  useEffect(() => {
    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible" && viewingMissionId) {
        reloadMissionHistory(viewingMissionId);
      }
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
  }, [viewingMissionId, reloadMissionHistory]);

  // Periodically sync history for running missions to catch missed SSE events
  useEffect(() => {
    if (!viewingMissionId || !viewingMissionIsRunning) return;
    const interval = setInterval(() => {
      if (document.visibilityState === "visible") {
        reloadMissionHistory(viewingMissionId);
      }
    }, 15_000);
    return () => clearInterval(interval);
  }, [viewingMissionId, viewingMissionIsRunning, reloadMissionHistory]);

  // Compute queued items for the queue strip
  const queuedItems: QueueItem[] = useMemo(() => {
    return items
      .filter((item): item is Extract<typeof item, { kind: "user" }> =>
        item.kind === "user" && item.queued === true
      )
      .map((item) => ({
        id: item.id,
        content: item.content,
        agent: null, // Agent info not stored in current item structure
      }));
  }, [items]);

  // Handle removing a message from the queue
  const handleRemoveFromQueue = async (messageId: string) => {
    try {
      await removeFromQueue(messageId);
      // Optimistically remove from local state
      setItems((prev) => prev.filter((item) => item.id !== messageId));
      toast.success("Removed from queue");
    } catch (err) {
      console.error(err);
      toast.error("Failed to remove from queue");
    }
  };

  // Handle clearing all queued messages
  const handleClearQueue = async () => {
    try {
      const { cleared } = await clearQueue();
      // Optimistically remove all queued items from local state
      setItems((prev) => prev.filter((item) => !(item.kind === "user" && item.queued === true)));
      toast.success(`Cleared ${cleared} message${cleared !== 1 ? "s" : ""} from queue`);
    } catch (err) {
      console.error(err);
      toast.error("Failed to clear queue");
    }
  };

  const activeMission = viewingMission ?? currentMission;
  const workspaceNameById = useMemo(() => {
    return Object.fromEntries(workspaces.map((ws) => [ws.id, ws.name]));
  }, [workspaces]);
  const activeWorkspaceLabel = activeMission?.workspace_name
    || (activeMission?.workspace_id ? workspaceNameById[activeMission.workspace_id] : undefined);
  const missionStatus = activeMission
    ? missionStatusLabel(activeMission.status)
    : null;

  // Derive the last resolved model from assistant messages (for the debug dropdown)
  const lastResolvedModel = useMemo(() => {
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i];
      if (it.kind === "assistant" && it.model) return it.model;
    }
    return null;
  }, [items]);

  // Determine if we should show the resume UI for interrupted/blocked/failed missions
  // Don't show resume UI if:
  // - Mission is running
  // - Last turn completed (assistant message at end - ready for user input)
  // - User just sent a message (waiting for assistant response)
  // Note: For failed missions, we show resume even if lastTurnCompleted (error message is last)
  const lastItem = lastNonQueuedItem ?? items[items.length - 1];
  const lastTurnCompleted = lastItem?.kind === 'assistant';
  const waitingForResponse = lastItem?.kind === 'user';
  const isFailed = activeMission?.status === 'failed';
  const showResumeUI = activeMission &&
    !viewingMissionIsRunning &&
    !waitingForResponse &&
    !dismissedResumeUI &&
    (isFailed || (!lastTurnCompleted && (activeMission.status === 'interrupted' || activeMission.status === 'blocked')));

  // Reset dismissedResumeUI when switching missions
  useEffect(() => {
    setDismissedResumeUI(false);
  }, [activeMission?.id]);

  return (
    <div className="flex h-screen flex-col p-6">
      {/* Hidden file input */}
      <input
        ref={fileInputRef}
        type="file"
        multiple
        onChange={handleFileChange}
        className="hidden"
      />

      {/* Mission Switcher Command Palette */}
      <MissionSwitcher
        open={showMissionSwitcher}
        onClose={() => setShowMissionSwitcher(false)}
        missions={recentMissions}
        runningMissions={runningMissions}
        currentMissionId={currentMission?.id}
        viewingMissionId={viewingMissionId}
        workspaceNameById={workspaceNameById}
        onSelectMission={handleViewMission}
        onCancelMission={handleCancelMission}
        onRefresh={refreshRecentMissions}
      />

      <MissionAutomationsDialog
        open={showAutomationsDialog}
        missionId={activeMission?.id ?? null}
        missionLabel={
          activeMission
            ? activeWorkspaceLabel
              ? `${activeWorkspaceLabel} · ${getMissionShortName(activeMission.id)}`
              : getMissionShortName(activeMission.id)
            : null
        }
        onClose={() => setShowAutomationsDialog(false)}
      />

      {/* Header */}
      <div className="relative z-10 mb-6 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          {/* Unified Mission Selector */}
          <div className="relative">
            <button
              onClick={() => setShowMissionSwitcher(true)}
              className={cn(
                "flex h-9 items-center gap-2 px-3 rounded-lg transition-colors",
                "bg-indigo-500/20 hover:bg-indigo-500/30"
              )}
              title="Switch mission (⌘K)"
            >
              {activeMission ? (
                <>
                  <div
                    className={cn(
                      "h-2 w-2 rounded-full shrink-0",
                      missionStatusDotClass(activeMission.status)
                    )}
                    title={missionStatus?.label}
                  />
                  {activeWorkspaceLabel && (
                    <>
                      <span className="text-sm font-medium text-white/50 truncate max-w-[160px] sm:max-w-[220px]">
                        {activeWorkspaceLabel}
                      </span>
                      <span className="text-white/40">·</span>
                    </>
                  )}
                  <span className="text-sm font-medium text-white/70 truncate max-w-[140px] sm:max-w-[180px]">
                    {getMissionShortName(activeMission.id)}
                  </span>
                </>
              ) : (
                <>
                  <Layers className="h-4 w-4 text-indigo-400" />
                  <span className="text-sm font-medium text-white/50">No mission</span>
                </>
              )}
              <ChevronDown className="h-3 w-3 text-white/40" />
            </button>
          </div>
        </div>

        <div className="flex items-center gap-3 shrink-0">
          <NewMissionDialog
            workspaces={workspaces}
            disabled={missionLoading}
            onCreate={handleNewMission}
            initialValues={activeMission ? {
              workspaceId: activeMission.workspace_id,
              agent: activeMission.agent,
              backend: activeMission.backend,
              modelOverride: activeMission.model_override,
              modelEffort: activeMission.model_effort,
            } : undefined}
          />

          <button
            onClick={() => setShowAutomationsDialog(true)}
            disabled={!activeMission}
            className={cn(
              "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
              activeMission
                ? "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]"
                : "border-white/[0.04] bg-white/[0.01] text-white/30 cursor-not-allowed"
            )}
            title={activeMission ? "Manage mission automations" : "Select a mission to manage automations"}
          >
            <Clock className="h-4 w-4" />
            <span className="hidden sm:inline">Automations</span>
          </button>

          {/* Thinking panel toggle */}
          <button
            onClick={handleToggleThinkingPanel}
            className={cn(
              "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
              showThinkingPanel
                ? "border-indigo-500/30 bg-indigo-500/10 text-indigo-400"
                : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]",
              hasActiveThinking && !showThinkingPanel && "border-indigo-500/50 animate-pulse-subtle"
            )}
            title={showThinkingPanel ? "Hide thinking panel" : "Show thinking panel"}
          >
            <Brain className={cn("h-4 w-4", hasActiveThinking && "animate-pulse")} />
            <span className="hidden sm:inline">Thinking</span>
            {thinkingItemsCount > 0 && (
              <span className="text-xs opacity-60">({thinkingItemsCount})</span>
            )}
          </button>

          {/* Desktop stream toggle with display selector - only shown when a desktop session is active */}
          {hasDesktopSession && (
            <div className="relative flex items-center">
              <button
                onClick={() => setShowDesktopStream(!showDesktopStream)}
                className={cn(
                  "flex items-center gap-2 rounded-l-lg border px-3 py-2 text-sm transition-colors",
                  showDesktopStream
                    ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-400"
                    : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]"
                )}
                title={showDesktopStream ? "Hide desktop stream" : "Show desktop stream"}
              >
                <Monitor className="h-4 w-4" />
                <span className="hidden sm:inline">Desktop</span>
                {showDesktopStream ? (
                  <PanelRightClose className="h-4 w-4" />
                ) : (
                  <PanelRight className="h-4 w-4" />
                )}
              </button>
              <div className="relative">
                <button
                  onClick={() => setShowDisplaySelector(!showDisplaySelector)}
                  className={cn(
                    "flex items-center gap-1.5 rounded-r-lg border-y border-r px-3 py-2 text-sm transition-colors",
                    showDesktopStream
                      ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-400"
                      : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]"
                  )}
                  title="Select display"
                >
                  <span className="text-sm font-mono">{desktopDisplayId}</span>
                  <ChevronDown className="h-3.5 w-3.5" />
                </button>
                {showDisplaySelector && (
                  <div className="absolute right-0 top-full mt-1 z-50 min-w-[280px] rounded-lg border border-white/[0.06] bg-[#121214] shadow-xl">
                    {/* Show sessions from API if available, otherwise show hardcoded list */}
                    {desktopSessions.length > 0 ? (
                      <>
                        {desktopSessions.map((session, index) => (
                          <div
                            key={`${session.display}-${session.mission_id || index}`}
                            className={cn(
                              "flex w-full items-center gap-2 px-3 py-2 text-sm transition-colors hover:bg-white/[0.04]",
                              desktopDisplayId === session.display
                                ? "bg-white/[0.02]"
                                : ""
                            )}
                          >
                            <button
                              onClick={() => {
                                setDesktopDisplayId(session.display);
                                setShowDisplaySelector(false);
                              }}
                              className="flex flex-1 items-center gap-2 text-left"
                            >
                              {/* Status indicator */}
                              <span className={cn(
                                "h-2 w-2 rounded-full",
                                !session.process_running ? "bg-gray-600" :
                                session.status === 'active' ? "bg-emerald-500" :
                                session.status === 'orphaned' ? "bg-amber-500" :
                                "bg-gray-500"
                              )} title={session.process_running ? session.status : 'stopped'} />

                              {/* Display ID */}
                              <span className={cn(
                                "font-mono",
                                desktopDisplayId === session.display
                                  ? "text-emerald-400"
                                  : "text-white/70"
                              )}>
                                {session.display}
                              </span>

                              {/* Status label */}
                              <span className={cn(
                                "text-xs",
                                !session.process_running ? "text-white/30" :
                                session.status === 'active' ? "text-emerald-500/70" :
                                session.status === 'orphaned' ? "text-amber-500/70" :
                                "text-white/40"
                              )}>
                                {!session.process_running ? 'Stopped' :
                                 session.status === 'active' ? 'Active' :
                                 session.status === 'orphaned' ? 'Orphaned' :
                                 session.status}
                              </span>

                              {/* Auto-close countdown for orphaned sessions */}
                              {session.status === 'orphaned' && session.auto_close_in_secs != null && session.auto_close_in_secs > 0 && (
                                <span className="text-xs text-amber-500/50">
                                  {Math.floor(session.auto_close_in_secs / 60)}m left
                                </span>
                              )}

                              {desktopDisplayId === session.display && (
                                <CheckCircle className="ml-auto h-3.5 w-3.5 text-emerald-400" />
                              )}
                            </button>

                            {/* Keep alive button for orphaned sessions */}
                            {session.status === 'orphaned' && (
                              <button
                                onClick={(e) => {
                                  e.stopPropagation();
                                  handleKeepAliveDesktopSession(session.display);
                                }}
                                className="p-1 text-white/40 hover:text-amber-400 transition-colors"
                                title="Extend keep-alive (+2h)"
                              >
                                <Clock className="h-3.5 w-3.5" />
                              </button>
                            )}

                            {/* Close button */}
                            <button
                              onClick={(e) => {
                                e.stopPropagation();
                                handleCloseDesktopSession(session.display);
                              }}
                              disabled={isClosingDesktop === session.display}
                              className={cn(
                                "p-1 transition-colors",
                                isClosingDesktop === session.display
                                  ? "text-white/20"
                                  : "text-white/40 hover:text-red-400"
                              )}
                              title="Close session"
                            >
                              {isClosingDesktop === session.display ? (
                                <Loader className="h-3.5 w-3.5 animate-spin" />
                              ) : (
                                <X className="h-3.5 w-3.5" />
                              )}
                            </button>
                          </div>
                        ))}

                        {/* Separator and cleanup action if there are orphaned sessions */}
                        {desktopSessions.some(s => s.status === 'orphaned' && s.process_running) && (
                          <>
                            <div className="my-1 h-px bg-white/[0.06]" />
                            <button
                              onClick={async () => {
                                try {
                                  await cleanupOrphanedDesktopSessions();
                                  toast.success('Orphaned sessions cleaned up');
                                  await refreshDesktopSessions();
                                } catch (err) {
                                  toast.error('Failed to cleanup sessions');
                                }
                              }}
                              className="flex w-full items-center gap-2 px-3 py-2 text-xs text-amber-500/70 hover:bg-white/[0.04] transition-colors"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                              Close all orphaned
                            </button>
                          </>
                        )}

                        {/* Separator and cleanup action if there are stopped sessions */}
                        {desktopSessions.some(s => !s.process_running || s.status === 'stopped') && (
                          <>
                            <div className="my-1 h-px bg-white/[0.06]" />
                            <button
                              onClick={async () => {
                                try {
                                  await cleanupStoppedDesktopSessions();
                                  toast.success('Stopped sessions cleared');
                                  await refreshDesktopSessions();
                                } catch (err) {
                                  toast.error('Failed to clear stopped sessions');
                                }
                              }}
                              className="flex w-full items-center gap-2 px-3 py-2 text-xs text-white/40 hover:bg-white/[0.04] transition-colors"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                              Clear stopped sessions
                            </button>
                          </>
                        )}
                      </>
                    ) : (
                      /* Fallback to hardcoded list if no sessions from API */
                      [":99", ":100", ":101", ":102"].map((display) => (
                        <button
                          key={display}
                          onClick={() => {
                            setDesktopDisplayId(display);
                            setShowDisplaySelector(false);
                          }}
                          className={cn(
                            "flex w-full items-center px-3 py-2 text-sm font-mono transition-colors hover:bg-white/[0.04]",
                            desktopDisplayId === display
                              ? "text-emerald-400"
                              : "text-white/70"
                          )}
                        >
                          {display}
                          {desktopDisplayId === display && (
                            <CheckCircle className="ml-auto h-3.5 w-3.5" />
                          )}
                        </button>
                      ))
                    )}
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Status panel */}
          <div className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2">
            {/* Connection status indicator - only show when not connected */}
            {connectionState !== "connected" && (
              <>
                <div className={cn(
                  "flex items-center gap-2",
                  connectionState === "reconnecting" ? "text-amber-400" : "text-red-400"
                )}>
                  {connectionState === "reconnecting" ? (
                    <>
                      <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                      <span className="text-sm font-medium">
                        Reconnecting{reconnectAttempt > 1 ? ` (${reconnectAttempt})` : "..."}
                      </span>
                    </>
                  ) : (
                    <>
                      <WifiOff className="h-3.5 w-3.5" />
                      <span className="text-sm font-medium">Disconnected</span>
                    </>
                  )}
                </div>
                <div className="h-4 w-px bg-white/[0.08]" />
              </>
            )}

            {/* Run state indicator with debug dropdown */}
            <div className="relative">
              <button
                onClick={() => setShowStreamDiagnostics((prev) => !prev)}
                className={cn(
                  "flex items-center gap-2 rounded-md px-2 py-1 transition-colors hover:bg-white/[0.04]",
                  status.className
                )}
                title="Click for debug info"
              >
                <StatusIcon
                  className={cn(
                    "h-3.5 w-3.5",
                    viewingRunState !== "idle" && "animate-spin"
                  )}
                />
                <span className="text-sm font-medium">{status.label}</span>
              </button>

              {showStreamDiagnostics && (
                <div className="absolute right-0 top-full z-50 mt-2 w-[280px] rounded-lg border border-white/[0.08] bg-[#121214] p-2.5 shadow-xl">
                  {/* Mission Info */}
                  {activeMission && (
                    <div className="space-y-0.5 text-xs">
                      <div className="flex items-center justify-between gap-2">
                        <span className="text-white/40">Mission</span>
                        <span className="font-mono text-[11px] text-white/60 select-all">
                          {activeMission.id.slice(0, 8)}
                        </span>
                      </div>
                      {activeMission.workspace_name && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Workspace</span>
                          <span className="font-mono text-white/80">{activeMission.workspace_name}</span>
                        </div>
                      )}
                      {activeMission.agent && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Agent</span>
                          <span className="font-mono text-white/80">{activeMission.agent}</span>
                        </div>
                      )}
                      {activeMission.backend && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Backend</span>
                          <span className="font-mono text-white/80">{activeMission.backend}</span>
                        </div>
                      )}
                      {activeMission.model_override && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Model override</span>
                          <span className="font-mono text-[11px] text-indigo-400 truncate max-w-[160px]" title={activeMission.model_override}>
                            {activeMission.model_override}
                          </span>
                        </div>
                      )}
                      {activeMission.model_effort && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Model effort</span>
                          <span className="font-mono text-[11px] text-amber-300 truncate max-w-[160px]" title={activeMission.model_effort}>
                            {activeMission.model_effort}
                          </span>
                        </div>
                      )}
                      {lastResolvedModel && (
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-white/40">Resolved model</span>
                          <span className="font-mono text-[11px] text-emerald-400 truncate max-w-[160px]" title={lastResolvedModel}>
                            {lastResolvedModel}
                          </span>
                        </div>
                      )}
                    </div>
                  )}

                  {/* Stream Status */}
                  <div className={cn("space-y-0.5 text-xs", activeMission && "mt-2 pt-2 border-t border-white/[0.06]")}>
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-white/40">Stream</span>
                      <span className="flex items-center gap-1.5 font-mono text-white/80">
                        <span
                          className={cn(
                            "h-1.5 w-1.5 rounded-full",
                            (streamDiagnostics.phase === "streaming" || streamDiagnostics.phase === "open") && "bg-emerald-400",
                            streamDiagnostics.phase === "connecting" && "bg-amber-400",
                            streamDiagnostics.phase === "error" && "bg-red-400",
                            (streamDiagnostics.phase === "closed" || streamDiagnostics.phase === "idle") && "bg-white/30"
                          )}
                        />
                        {streamDiagnostics.phase}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-white/40">Activity</span>
                      <span className="font-mono text-white/60 text-[11px]">
                        {formatDiagAge(streamDiagnostics.lastEventAt)}
                      </span>
                    </div>
                  </div>

                  {streamDiagnostics.lastError && (
                    <div className="mt-2 rounded border border-red-500/30 bg-red-500/10 px-2 py-1 text-[11px] text-red-300">
                      {streamDiagnostics.lastError}
                    </div>
                  )}

                  {streamHints.length > 0 && (
                    <div className="mt-2 space-y-0.5 rounded border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-200">
                      {streamHints.map((hint) => (
                        <div key={hint}>{hint}</div>
                      ))}
                    </div>
                  )}

                  <button
                    onClick={handleCopyDiagnostics}
                    className="mt-2 w-full text-center text-[11px] text-white/40 hover:text-white/70 transition-colors"
                  >
                    Copy debug info
                  </button>
                </div>
              )}
            </div>

            {/* Queue count */}
            <div className="h-4 w-px bg-white/[0.08]" />
            <div
              className="flex items-center gap-1.5"
              title={viewingQueueLen > 0 ? `${viewingQueueLen} message${viewingQueueLen > 1 ? 's' : ''} waiting to be processed` : 'No messages queued'}
            >
              <span className="text-[10px] uppercase tracking-wider text-white/40">
                Queue
              </span>
              <span className={cn(
                "text-sm font-medium tabular-nums",
                viewingQueueLen === 0 && "text-white/70",
                viewingQueueLen > 0 && viewingQueueLen < 3 && "text-amber-400",
                viewingQueueLen >= 3 && "text-orange-400"
              )}>
                {viewingQueueLen}
              </span>
            </div>

            {/* Progress indicator */}
            {viewingProgress && viewingProgress.total > 0 && (
              <>
                <div className="h-4 w-px bg-white/[0.08]" />
                <div className="flex items-center gap-1.5">
                  <span className="text-[10px] uppercase tracking-wider text-white/40">
                    Subtask
                  </span>
                  <span className="text-sm font-medium text-emerald-400 tabular-nums">
                    {Math.min(viewingProgress.completed + 1, viewingProgress.total)}/{viewingProgress.total}
                  </span>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Main content area - Chat and Desktop stream side by side */}
      <div className="flex-1 min-h-0 flex gap-4">
        {/* Chat container */}
        <div className={cn(
          "flex-1 min-h-0 flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden relative transition-all duration-300",
          showDesktopStream && "flex-[2]"
        )}>
        {/* Messages */}
        <div ref={containerRef} className="flex-1 overflow-y-auto p-6">
          {items.length === 0 ? (
            <div className="flex h-full items-center justify-center">
              <div className="text-center">
                <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10">
                  {viewingMissionIsRunning ? (
                    <Loader className="h-8 w-8 text-indigo-400 animate-spin" />
                  ) : (
                    <Bot className="h-8 w-8 text-indigo-400" />
                  )}
                </div>
                {missionLoading ? (
                  <Shimmer className="max-w-xs mx-auto" />
                ) : viewingMissionIsRunning ? (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      Agent is working...
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      Processing your request. Updates will appear here as they
                      arrive.
                    </p>
                  </>
                ) : activeMission && activeMission.status !== "active" ? (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      {activeMission.status === "interrupted" 
                        ? "Mission Interrupted" 
                        : activeMission.status === "blocked"
                        ? "Iteration Limit Reached"
                        : "No conversation history"}
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      {activeMission.status === "interrupted" ? (
                        <>This mission was interrupted (server shutdown or cancellation). Click the <strong className="text-amber-400">Resume</strong> button in the mission menu to continue where you left off.</>
                      ) : activeMission.status === "blocked" ? (
                        <>The agent reached its iteration limit ({maxIterations}). You can continue the mission to give it more iterations.</>
                      ) : activeMission.status === "failed" ? (
                        <>This mission failed without producing any messages.</>
                      ) : activeMission.status === "not_feasible" ? (
                        <>The agent determined this task was not feasible.</>
                      ) : (
                        <>This mission was {activeMission.status} without any messages.
                        {activeMission.status === "completed" && " You can reactivate it to continue."}</>
                      )}
                    </p>
                    {activeMission.status === "blocked" && (
                      <div className="mt-4 flex gap-2">
                        <button
                          onClick={() => handleResumeMission()}
                          disabled={missionLoading}
                          className="inline-flex items-center gap-2 rounded-lg bg-indigo-500 px-4 py-2 text-sm font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                        >
                          {missionLoading ? (
                            <Loader className="h-4 w-4 animate-spin" />
                          ) : (
                            <PlayCircle className="h-4 w-4" />
                          )}
                          Continue Mission
                        </button>
                      </div>
                    )}
                  </>
                ) : (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      Start a conversation
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      Ask the agent to do something — messages queue while
                      it&apos;s busy
                    </p>

                    <p className="mt-4 text-xs text-white/30">
                      Tip: Paste files directly to upload to context folder
                    </p>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="mx-auto max-w-3xl space-y-6">
              {/* Performance: only render recent items, with option to load more */}
              {groupedItems.length > visibleItemsLimit && (
                <button
                  onClick={() => setVisibleItemsLimit(prev => prev + LOAD_MORE_INCREMENT)}
                  className="w-full py-2 px-4 text-sm text-white/50 hover:text-white/80 hover:bg-white/5 rounded-lg transition-colors flex items-center justify-center gap-2"
                >
                  <ChevronUp className="w-4 h-4" />
                  Load {Math.min(LOAD_MORE_INCREMENT, groupedItems.length - visibleItemsLimit)} older messages
                  <span className="text-white/30">
                    ({groupedItems.length - visibleItemsLimit} hidden)
                  </span>
                </button>
              )}
              {groupedItems.slice(-visibleItemsLimit).map((item) => {
                // Handle tool groups (multiple consecutive tools collapsed)
                if (item.kind === "tool_group") {
                  const isExpanded = expandedToolGroups.has(item.groupId);
                  return (
                    <CollapsedToolGroup
                      key={item.groupId}
                      tools={item.tools}
                      isExpanded={isExpanded}
                      onToggleExpand={() => {
                        // Use startTransition so expanding many tools
                        // doesn't block the browser from painting the
                        // button's click feedback first (issue #156).
                        startTransition(() => {
                          setExpandedToolGroups((prev) => {
                            const next = new Set(prev);
                            if (next.has(item.groupId)) {
                              next.delete(item.groupId);
                            } else {
                              next.add(item.groupId);
                            }
                            return next;
                          });
                        });
                      }}
                      workspaceId={missionForDownloads?.workspace_id}
                      missionId={missionForDownloads?.id}
                    />
                  );
                }

                if (item.kind === "user") {
                  return (
                    <div key={item.id} className="flex justify-end gap-3 group">
                      <CopyButton
                        text={item.content}
                        className="self-start mt-2"
                      />
                      <div className="max-w-[80%]">
                        <div
                          className={cn(
                            "rounded-2xl rounded-tr-md px-4 py-3 text-white selection-light",
                            item.queued
                              ? "border-2 border-dashed border-indigo-500/60 bg-indigo-500/20"
                              : "bg-indigo-500"
                          )}
                        >
                          <p className="whitespace-pre-wrap text-sm break-words">
                            {item.content}
                          </p>
                        </div>
                        <div className="mt-1 text-right flex items-center justify-end gap-2">
                          {item.queued === true && (
                            <span className="text-[10px] text-white/30">
                              Queued
                            </span>
                          )}
                          <span className="text-[10px] text-white/30">
                            {formatTime(item.timestamp)}
                          </span>
                        </div>
                      </div>
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.08]">
                        <User className="h-4 w-4 text-white/60" />
                      </div>
                    </div>
                  );
                }

                if (item.kind === "assistant") {
                  const statusIcon = item.success ? CheckCircle : XCircle;
                  const MessageStatusIcon = statusIcon;
                  const displayModel = item.model
                    ? item.model.includes("/")
                      ? item.model.split("/").pop()
                      : item.model
                    : null;
                  return (
                    <div
                      key={item.id}
                      className="flex justify-start gap-3 group"
                    >
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                        <Bot className="h-4 w-4 text-indigo-400" />
                      </div>
                      <div className="max-w-[80%] rounded-2xl rounded-tl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                        <div className="mb-2 flex items-center gap-2 text-xs text-white/40">
                          <MessageStatusIcon
                            className={cn(
                              "h-3 w-3",
                              item.success ? "text-emerald-400" : "text-red-400"
                            )}
                          />
                          <span>{item.success ? "Completed" : "Failed"}</span>
                          {displayModel && (
                            <>
                              <span>•</span>
                              <span
                                className="font-mono truncate max-w-[120px]"
                                title={item.model ?? undefined}
                              >
                                {displayModel}
                              </span>
                            </>
                          )}
                          <>
                            <span>•</span>
                            <span
                              className={
                                item.costSource === "actual"
                                  ? "text-emerald-400"
                                  : item.costSource === "estimated"
                                    ? "text-amber-300"
                                    : "text-white/50"
                              }
                            >
                              {item.costSource === "unknown"
                                ? item.costCents > 0
                                  ? `$${(item.costCents / 100).toFixed(4)}`
                                  : "N/A"
                                : `$${(item.costCents / 100).toFixed(4)}`}
                            </span>
                            <span className="rounded bg-white/10 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-white/60">
                              {item.costSource === "actual"
                                ? "Actual"
                                : item.costSource === "estimated"
                                  ? "Estimated"
                                  : "Unknown"}
                            </span>
                          </>
                          <span>•</span>
                          <span className="text-white/30">
                            {formatTime(item.timestamp)}
                          </span>
                        </div>
                        <MarkdownContent
                          content={item.content}
                          basePath={missionWorkingDirectory}
                          workspaceId={missionForDownloads?.workspace_id}
                          missionId={missionForDownloads?.id}
                        />
                        {/* Render shared files */}
                        {item.sharedFiles && item.sharedFiles.length > 0 && (
                          <div className="mt-2">
                            {item.sharedFiles.map((file, idx) => (
                              <SharedFileCard key={`${file.url}-${idx}`} file={file} />
                            ))}
                          </div>
                        )}
                        {/* Resume button for failed messages */}
                        {!item.success && item.resumable && (
                          <div className="mt-3 flex gap-2">
                            <button
                              onClick={() => handleResumeMission()}
                              className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                            >
                              <RotateCcw className="h-3 w-3" />
                              Resume Mission
                            </button>
                          </div>
                        )}
                      </div>
                      <CopyButton
                        text={item.content}
                        className="self-start mt-8"
                      />
                    </div>
                  );
                }

                if (item.kind === "phase") {
                  return <PhaseItem key={item.id} item={item} />;
                }

                if (item.kind === "thinking_group") {
                  // Render grouped thinking items as a single merged block
                  return (
                    <ThinkingGroupItem
                      key={item.groupId}
                      items={item.thoughts}
                      basePath={missionWorkingDirectory}
                      workspaceId={missionForDownloads?.workspace_id}
                      missionId={missionForDownloads?.id}
                    />
                  );
                }

                if (item.kind === "thinking") {
                  // Fallback for individual thinking items (should be rare with grouping)
                  return (
                    <ThinkingGroupItem
                      key={item.id}
                      items={[item]}
                      basePath={missionWorkingDirectory}
                      workspaceId={missionForDownloads?.workspace_id}
                      missionId={missionForDownloads?.id}
                    />
                  );
                }

                if (item.kind === "stream") {
                  // Fallback for individual stream items (should be rare with grouping)
                  return (
                    <ThinkingGroupItem
                      key={item.id}
                      items={[item]}
                      basePath={missionWorkingDirectory}
                      workspaceId={missionForDownloads?.workspace_id}
                      missionId={missionForDownloads?.id}
                    />
                  );
                }

                if (item.kind === "tool") {
                  // UI tools get special interactive rendering
                  if (item.isUiTool) {
                    if (item.name === "question" || item.name === "AskUserQuestion") {
                      return (
                        <QuestionToolItem
                          key={item.id}
                          item={item}
                          onSubmit={async (toolCallId, answers) => {
                            setItems((prev) =>
                              prev.map((it) =>
                                it.kind === "tool" && it.toolCallId === toolCallId
                                  ? { ...it, result: { answers } }
                                  : it
                              )
                            );
                            await postControlToolResult({
                              tool_call_id: toolCallId,
                              name: item.name,
                              result: { answers },
                            });
                          }}
                        />
                      );
                    }
                    if (item.name === "ui_optionList") {
                      const toolCallId = item.toolCallId;
                      const rawArgs: Record<string, unknown> = isRecord(item.args)
                        ? item.args
                        : {};

                      let optionList: ReturnType<
                        typeof parseSerializableOptionList
                      > | null = null;
                      let parseErr: string | null = null;
                      try {
                        optionList = parseSerializableOptionList({
                          ...rawArgs,
                          id:
                            typeof rawArgs["id"] === "string" && rawArgs["id"]
                              ? (rawArgs["id"] as string)
                              : `option-list-${toolCallId}`,
                        });
                      } catch (e) {
                        parseErr =
                          e instanceof Error
                            ? e.message
                            : "Invalid option list payload";
                      }

                      const confirmed = item.result as
                        | OptionListSelection
                        | undefined;

                      return (
                        <div key={item.id} className="flex justify-start gap-3">
                          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                            <Bot className="h-4 w-4 text-indigo-400" />
                          </div>
                          <div className="max-w-[80%] rounded-2xl rounded-tl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                            <div className="mb-2 text-xs text-white/40">
                              Tool:{" "}
                              <span className="font-mono text-indigo-400">
                                {item.name}
                              </span>
                            </div>

                            {parseErr || !optionList ? (
                              <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-400">
                                {parseErr ?? "Failed to render OptionList"}
                              </div>
                            ) : (
                              <OptionListErrorBoundary>
                                <OptionList
                                  {...optionList}
                                  value={undefined}
                                  confirmed={confirmed}
                                  onConfirm={async (selection) => {
                                    setItems((prev) =>
                                      prev.map((it) =>
                                        it.kind === "tool" &&
                                        it.toolCallId === toolCallId
                                          ? { ...it, result: selection }
                                          : it
                                      )
                                    );
                                    await postControlToolResult({
                                      tool_call_id: toolCallId,
                                      name: item.name,
                                      result: selection,
                                    });
                                  }}
                                  onCancel={async () => {
                                    setItems((prev) =>
                                      prev.map((it) =>
                                        it.kind === "tool" &&
                                        it.toolCallId === toolCallId
                                          ? { ...it, result: null }
                                          : it
                                      )
                                    );
                                    await postControlToolResult({
                                      tool_call_id: toolCallId,
                                      name: item.name,
                                      result: null,
                                    });
                                  }}
                                />
                              </OptionListErrorBoundary>
                            )}
                          </div>
                        </div>
                      );
                    }

                    if (item.name === "ui_dataTable") {
                      const rawArgs: Record<string, unknown> = isRecord(item.args)
                        ? item.args
                        : {};
                      const dataTable = parseSerializableDataTable(rawArgs);

                      return (
                        <div key={item.id} className="flex justify-start gap-3">
                          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                            <Bot className="h-4 w-4 text-indigo-400" />
                          </div>
                          <div className="max-w-[90%] rounded-2xl rounded-tl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                            <div className="mb-2 text-xs text-white/40">
                              Tool:{" "}
                              <span className="font-mono text-indigo-400">
                                {item.name}
                              </span>
                            </div>
                            {dataTable ? (
                              <DataTable
                                id={dataTable.id}
                                title={dataTable.title}
                                columns={dataTable.columns}
                                rows={dataTable.rows}
                              />
                            ) : (
                              <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-400">
                                Failed to render DataTable
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    }

                    // Unknown UI tool - still show with ToolCallItem
                    return (
                      <ToolCallItem
                        key={item.id}
                        item={item}
                        workspaceId={missionForDownloads?.workspace_id}
                        missionId={missionForDownloads?.id}
                      />
                    );
                  }

                  // Subagent/background task tools get enhanced rendering
                  if (isSubagentTool(item.name)) {
                  return <SubagentToolItem key={item.id} item={item} />;
                  }

                  // Non-UI tools use the collapsible ToolCallItem component
                  return (
                    <ToolCallItem
                      key={item.id}
                      item={item}
                      workspaceId={missionForDownloads?.workspace_id}
                      missionId={missionForDownloads?.id}
                    />
                  );
                }

                // system
                return (
                  <div key={item.id} className="flex justify-start gap-3">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.04]">
                      <Ban className="h-4 w-4 text-white/40" />
                    </div>
                    <div className="max-w-[80%] rounded-2xl rounded-tl-md bg-white/[0.02] border border-white/[0.04] px-4 py-3">
                      <p className="whitespace-pre-wrap text-sm text-white/60 break-words">
                        {item.content}
                      </p>
                      {item.resumable && (
                        <div className="mt-3 flex gap-2">
                          <button
                            onClick={() => handleResumeMission()}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                          >
                            <RotateCcw className="h-3 w-3" />
                            Resume Mission
                          </button>
                        </div>
                      )}
                    </div>
                  </div>
                );
              })}

              {/* Show streaming indicator when running but no active thinking/phase visible inline */}
              {viewingMissionIsRunning &&
                items.length > 0 &&
                !items.some(
                  (it) =>
                    // Only block for undone thinking if it's visible inline (panel closed)
                    ((it.kind === "thinking" || it.kind === "stream") &&
                      !it.done &&
                      !showThinkingPanel) ||
                    it.kind === "phase"
                ) &&
                // Hide if the last item is an assistant message (response complete, waiting for state change)
                items[items.length - 1]?.kind !== "assistant" && (
                  <div className="flex justify-start gap-3 animate-fade-in">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                      <Bot className="h-4 w-4 text-indigo-400 animate-pulse" />
                    </div>
                    <div className="rounded-2xl rounded-tl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                      <div className="flex items-center gap-2">
                        <Loader className="h-4 w-4 text-indigo-400 animate-spin" />
                        <span className="text-sm text-white/60">
                          Agent is working...
                        </span>
                      </div>
                    </div>
                  </div>
                )}

              {/* Waiting banner for question tool */}
              {hasPendingQuestion && (
                <div className="flex justify-center py-4 animate-fade-in">
                  <div className="flex flex-col sm:flex-row items-start sm:items-center gap-3 rounded-xl px-5 py-4 bg-indigo-500/10 border border-indigo-500/20">
                    <div className="flex items-center gap-3">
                      <HelpCircle className="h-5 w-5 shrink-0 text-indigo-300" />
                      <div className="text-sm">
                        <span className="font-medium text-indigo-200">
                          Waiting for your response
                        </span>
                        <p className="text-white/50">
                          The agent asked a question and is paused until you answer.
                        </p>
                      </div>
                    </div>
                  </div>
                </div>
              )}

              {/* Stall warning banner when agent hasn't reported activity for 60+ seconds */}
              {isViewingMissionStalled && viewingMissionId && !hasPendingQuestion && (
                <div className="flex justify-center py-4 animate-fade-in">
                  <div className={cn(
                    "flex flex-col sm:flex-row items-start sm:items-center gap-3 rounded-xl px-5 py-4",
                    isViewingMissionSeverelyStalled
                      ? "bg-red-500/10 border border-red-500/20"
                      : "bg-amber-500/10 border border-amber-500/20"
                  )}>
                    <div className="flex items-center gap-3">
                      <AlertTriangle className={cn(
                        "h-5 w-5 shrink-0",
                        isViewingMissionSeverelyStalled ? "text-red-400" : "text-amber-400"
                      )} />
                      <div className="text-sm">
                        <span className={cn(
                          "font-medium",
                          isViewingMissionSeverelyStalled ? "text-red-400" : "text-amber-400"
                        )}>
                          Agent may be stuck
                        </span>
                        <span className="text-white/50 ml-1">
                          — No activity for {Math.floor(viewingMissionStallSeconds)}s
                        </span>
                        <p className="text-white/40 text-xs mt-1">
                          {isViewingMissionSeverelyStalled
                            ? "The agent appears to be stuck on a long-running operation. Consider stopping it."
                            : "A tool or external operation may be taking longer than expected."}
                        </p>
                      </div>
                    </div>
                    <button
                      onClick={() => handleCancelMission(viewingMissionId)}
                      className={cn(
                        "shrink-0 inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium transition-colors",
                        isViewingMissionSeverelyStalled
                          ? "bg-red-500 text-white hover:bg-red-400"
                          : "bg-amber-500/20 text-amber-400 hover:bg-amber-500/30 border border-amber-500/30"
                      )}
                    >
                      <Square className="h-3.5 w-3.5" />
                      {isViewingMissionSeverelyStalled ? "Force Stop" : "Stop"}
                    </button>
                  </div>
                </div>
              )}

              {/* Continue banner for blocked missions */}
              {activeMission?.status === "blocked" && items.length > 0 && (
                <div className="flex justify-center py-4">
                  <div className="flex items-center gap-3 rounded-xl bg-amber-500/10 border border-amber-500/20 px-5 py-3">
                    <Clock className="h-5 w-5 text-amber-400" />
                    <div className="text-sm">
                      <span className="text-amber-400 font-medium">Iteration limit reached</span>
                      <span className="text-white/50 ml-1">— Agent used all {maxIterations} iterations</span>
                    </div>
                    <button
                      onClick={() => handleResumeMission()}
                      disabled={missionLoading}
                      className="ml-2 inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-sm font-medium text-black hover:bg-amber-400 transition-colors disabled:opacity-50"
                    >
                      {missionLoading ? (
                        <Loader className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <PlayCircle className="h-3.5 w-3.5" />
                      )}
                      Continue
                    </button>
                  </div>
                </div>
              )}

              <div ref={endRef} />
            </div>
          )}
        </div>

        {/* Scroll to bottom button */}
        {!isAtBottom && items.length > 0 && (
          <button
            onClick={() => scrollToBottom()}
            className="absolute bottom-20 right-6 p-2 rounded-full bg-white/[0.1] border border-white/[0.1] text-white/60 hover:bg-white/[0.15] hover:text-white/80 transition-all shadow-lg"
            title="Scroll to bottom"
          >
            <ArrowDown className="h-4 w-4" />
          </button>
        )}

        {/* Input */}
        <div className="border-t border-white/[0.06] bg-white/[0.01] p-4">
          {/* Upload progress */}
          {uploadProgress && (
            <div className="mx-auto max-w-3xl mb-3">
              <div className="flex items-center gap-3 rounded-lg border border-white/[0.06] bg-white/[0.02] px-4 py-3">
                <Loader className="h-4 w-4 animate-spin text-indigo-400" />
                <div className="flex-1 min-w-0">
                  <div className="flex items-center justify-between text-sm mb-1">
                    <span className="text-white/70 truncate">{uploadProgress.fileName}</span>
                    <span className="text-white/50 ml-2 shrink-0">
                      {formatBytes(uploadProgress.progress.loaded)} / {formatBytes(uploadProgress.progress.total)}
                    </span>
                  </div>
                  <div className="h-1.5 bg-white/[0.06] rounded-full overflow-hidden">
                    <div 
                      className="h-full bg-indigo-500 rounded-full transition-all duration-300"
                      style={{ width: `${uploadProgress.progress.percentage}%` }}
                    />
                  </div>
                </div>
                <span className="text-sm text-white/50 shrink-0">{uploadProgress.progress.percentage}%</span>
              </div>
            </div>
          )}

          {/* Upload queue (for files waiting) */}
          {uploadQueue.length > 0 && !uploadProgress && (
            <div className="mx-auto max-w-3xl mb-3 flex flex-wrap gap-2">
              {uploadQueue.map((name) => (
                <AttachmentPreview
                  key={name}
                  file={{ name, type: "" }}
                  isUploading
                />
              ))}
            </div>
          )}

          {/* URL Input */}
          {showUrlInput && (
            <div className="mx-auto max-w-3xl mb-3">
              <div className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2">
                <Link2 className="h-4 w-4 text-white/40 shrink-0" />
                <input
                  type="url"
                  value={urlInput}
                  onChange={(e) => setUrlInput(e.target.value)}
                  placeholder="Paste URL to download (Dropbox, Google Drive, direct link...)"
                  className="flex-1 bg-transparent text-sm text-white placeholder:text-white/30 focus:outline-none"
                  autoFocus
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      handleUrlDownload();
                    } else if (e.key === "Escape") {
                      setShowUrlInput(false);
                      setUrlInput("");
                    }
                  }}
                />
                {urlDownloading ? (
                  <Loader className="h-4 w-4 animate-spin text-indigo-400" />
                ) : (
                  <>
                    <button
                      type="button"
                      onClick={handleUrlDownload}
                      disabled={!urlInput.trim()}
                      className="text-sm text-indigo-400 hover:text-indigo-300 disabled:text-white/20 disabled:cursor-not-allowed"
                    >
                      Download
                    </button>
                    <button
                      type="button"
                      onClick={() => { setShowUrlInput(false); setUrlInput(""); }}
                      className="text-white/40 hover:text-white/70"
                    >
                      <X className="h-4 w-4" />
                    </button>
                  </>
                )}
              </div>
              <p className="text-xs text-white/30 mt-1.5 px-1">
                Server will download the file directly — faster for large files
              </p>
            </div>
          )}

          {/* Show resume buttons for interrupted/blocked missions, otherwise show normal input */}
          {/* Note: showResumeUI checks viewingMissionIsRunning and if the last turn completed */}
          {showResumeUI ? (
            <div className="mx-auto flex max-w-3xl gap-3 items-center justify-center py-2">
              <div className="flex items-center gap-2 text-sm text-white/50 mr-4">
                <AlertTriangle className="h-4 w-4 text-amber-400" />
                <span>Mission {activeMission.status === 'blocked' ? 'blocked' : activeMission.status === 'failed' ? 'failed' : 'interrupted'}</span>
              </div>
              <button
                onClick={() => handleResumeMission()}
                disabled={missionLoading}
                className="flex items-center gap-2 rounded-xl border border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04] px-5 py-3 text-sm font-medium text-white/70 transition-colors disabled:opacity-50"
              >
                <PlayCircle className="h-4 w-4" />
                {activeMission.status === 'blocked' ? 'Continue' : activeMission.status === 'failed' ? 'Retry' : 'Resume'}
              </button>
              <button
                onClick={() => setDismissedResumeUI(true)}
                className="flex items-center gap-2 rounded-xl border border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04] px-5 py-3 text-sm font-medium text-white/70 transition-colors"
              >
                <MessageSquare className="h-4 w-4" />
                Custom Message
              </button>
            </div>
          ) : (
            <div className="mx-auto max-w-3xl w-full space-y-2">
              {/* Queue Strip - shows queued messages when present */}
              <QueueStrip
                items={queuedItems}
                onRemove={handleRemoveFromQueue}
                onClearAll={handleClearQueue}
              />

              <form
                onSubmit={(e) => e.preventDefault()}
                className="flex gap-3 items-end"
              >
                <div className="flex gap-1">
                  <button
                    type="button"
                    onClick={() => fileInputRef.current?.click()}
                    className="p-3 rounded-xl border border-white/[0.06] bg-white/[0.02] text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors shrink-0"
                    title="Attach files"
                  >
                    <Paperclip className="h-5 w-5" />
                  </button>
                  <button
                    type="button"
                    onClick={() => setShowUrlInput(!showUrlInput)}
                    className={`p-3 rounded-xl border border-white/[0.06] bg-white/[0.02] text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors shrink-0 ${showUrlInput ? 'text-indigo-400 border-indigo-500/30' : ''}`}
                    title="Download from URL"
                  >
                    <Link2 className="h-5 w-5" />
                  </button>
                </div>

                <EnhancedInput
                  ref={enhancedInputRef}
                  value={input}
                  onChange={setInput}
                  onSubmit={handleEnhancedSubmit}
                  onCanSubmitChange={setCanSubmitInput}
                  onFilePaste={handleFilePaste}
                  placeholder="Message the root agent… (paste files to upload)"
                  backend={viewingMission?.backend ?? currentMission?.backend}
                />

                {isBusy ? (
                  <>
                    <button
                      type="button"
                      onClick={() => enhancedInputRef.current?.submit()}
                      disabled={!canSubmitInput}
                      className="flex items-center gap-2 rounded-xl bg-indigo-500/80 hover:bg-indigo-600 px-5 py-3 text-sm font-medium text-white transition-colors shrink-0 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-indigo-500/80"
                    >
                      <ListPlus className="h-4 w-4" />
                      Queue
                    </button>
                    <button
                      type="button"
                      onClick={handleStop}
                      className="flex items-center gap-2 rounded-xl bg-red-500 hover:bg-red-600 px-5 py-3 text-sm font-medium text-white transition-colors shrink-0"
                    >
                      <Square className="h-4 w-4" />
                      Stop
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={() => enhancedInputRef.current?.submit()}
                    disabled={!canSubmitInput}
                    className="flex items-center gap-2 rounded-xl bg-indigo-500 hover:bg-indigo-600 px-5 py-3 text-sm font-medium text-white transition-colors shrink-0 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-indigo-500"
                  >
                    <Send className="h-4 w-4" />
                    Send
                  </button>
              )}
              </form>
            </div>
          )}
        </div>
      </div>

        {/* Right column: Thinking Panel and Desktop Stream stacked */}
        {(showThinkingPanel || showDesktopStream) && (
          <div className={cn(
            "min-h-0 flex flex-col gap-4 transition-all duration-300 animate-fade-in shrink-0",
            showDesktopStream ? "flex-1 max-w-md" : "w-80"
          )}>
            {/* Thinking Panel */}
            {showThinkingPanel && (
              <ThinkingPanel
                items={thinkingItems}
                onClose={handleCloseThinkingPanel}
                className={showDesktopStream ? "flex-shrink-0 max-h-[40%]" : "flex-1"}
                basePath={missionWorkingDirectory}
                missionId={viewingMissionId}
              />
            )}

            {/* Desktop Stream Panel */}
            {showDesktopStream && (
              <div className={cn(
                "min-h-0",
                showThinkingPanel ? "flex-1" : "flex-1"
              )}>
                <DesktopStream
                  displayId={desktopDisplayId}
                  className="h-full"
                  onClose={() => setShowDesktopStream(false)}
                />
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
