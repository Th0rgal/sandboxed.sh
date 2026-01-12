"use client";

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { toast } from "@/components/toast";
import { MarkdownContent } from "@/components/markdown-content";
import { EnhancedInput, type SubmitPayload } from "@/components/enhanced-input";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { cn } from "@/lib/utils";
import { getRuntimeApiBase } from "@/lib/settings";
import { authHeader } from "@/lib/auth";
import {
  cancelControl,
  postControlMessage,
  postControlToolResult,
  streamControl,
  loadMission,
  getMission,
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
  listProviders,
  listWorkspaces,
  getHealth,
  listDesktopSessions,
  closeDesktopSession,
  keepAliveDesktopSession,
  cleanupOrphanedDesktopSessions,
  type StreamDiagnosticUpdate,
  type ControlRunState,
  type Mission,
  type MissionStatus,
  type RunningMissionInfo,
  type UploadProgress,
  type Provider,
  type Workspace,
  type DesktopSessionDetail,
  type DesktopSessionStatus,
} from "@/lib/api";
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
  X,
  Wrench,
  Terminal,
  FileText,
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
} from "lucide-react";

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
      multiple: Boolean(entry["multiple"]),
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

  if (file.kind === "image") {
    // Render images inline with click to open
    return (
      <div className="mt-3 rounded-lg overflow-hidden border border-white/[0.06] bg-black/20">
        <a
          href={file.url}
          target="_blank"
          rel="noopener noreferrer"
          className="block"
        >
          <img
            src={file.url}
            alt={file.name}
            className="max-w-full max-h-[400px] object-contain"
            loading="lazy"
          />
        </a>
        <div className="flex items-center gap-2 px-3 py-2 text-xs text-white/40 border-t border-white/[0.06]">
          <Image className="h-3 w-3" />
          <span className="truncate flex-1">{file.name}</span>
          {sizeLabel && <span>{sizeLabel}</span>}
          <a
            href={file.url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-indigo-400 hover:text-indigo-300 flex items-center gap-1"
          >
            <ExternalLink className="h-3 w-3" />
          </a>
        </div>
      </div>
    );
  }

  // Render other files as download cards
  return (
    <a
      href={file.url}
      target="_blank"
      rel="noopener noreferrer"
      download={file.name}
      className="mt-3 flex items-center gap-3 px-4 py-3 rounded-lg border border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04] transition-colors group"
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
      </div>
      <Download className="h-4 w-4 text-white/30 group-hover:text-indigo-400 transition-colors" />
    </a>
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
}: {
  items: Extract<ChatItem, { kind: "thinking" }>[];
  basePath?: string;
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

  const label = nonEmptyItems.length === 1 ? "Thought" : "Thoughts";

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
          {hasActiveItem ? `Thinking for ${duration}` : `${label} for ${duration}`}
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
                <MarkdownContent
                  content={item.content}
                  className="text-xs text-white/60 [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
                  basePath={basePath}
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
function ThinkingPanelItem({
  item,
  isActive,
  basePath,
}: {
  item: Extract<ChatItem, { kind: "thinking" }>;
  isActive: boolean;
  basePath?: string;
}) {
  const [elapsedSeconds, setElapsedSeconds] = useState(0);

  useEffect(() => {
    if (item.done) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [item.done, item.startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  const duration = item.done && item.endTime
    ? formatDuration(Math.floor((item.endTime - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  return (
    <div className={cn(
      "rounded-lg border p-3 transition-all",
      isActive
        ? "border-indigo-500/30 bg-indigo-500/5"
        : "border-white/[0.06] bg-white/[0.02]"
    )}>
      <div className="flex items-center gap-2 mb-2">
        <Brain
          className={cn(
            "h-3.5 w-3.5",
            isActive ? "animate-pulse text-indigo-400" : "text-white/40"
          )}
        />
        <span className={cn(
          "text-xs font-medium",
          isActive ? "text-indigo-400" : "text-white/50"
        )}>
          {isActive ? `Thinking for ${duration}` : `Thought for ${duration}`}
        </span>
      </div>
      <div className={cn(
        "text-xs leading-relaxed",
        isActive ? "text-white/70" : "text-white/40",
        "max-h-[50vh] overflow-y-auto"
      )}>
        {item.content ? (
          <MarkdownContent
            content={item.content}
            className="text-xs [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
            basePath={basePath}
          />
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
}: {
  items: Extract<ChatItem, { kind: "thinking" }>[];
  onClose: () => void;
  className?: string;
  basePath?: string;
}) {
  const activeItem = items.find(t => !t.done);

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

  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when active thought content changes
  useEffect(() => {
    if (scrollRef.current && activeItem) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [activeItem?.content, activeItem?.id]);

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
            activeItem ? "animate-pulse text-indigo-400" : "text-white/40"
          )} />
          <span className="text-sm font-medium text-white">
            {activeItem ? "Thinking" : "Thoughts"}
          </span>
          {(completedItems.length > 0 || activeItem) && (
            <span className="text-xs text-white/30">({completedItems.length + (activeItem ? 1 : 0)})</span>
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
                {completedItems.map((item) => (
                  <ThinkingPanelItem key={item.id} item={item} isActive={false} basePath={basePath} />
                ))}
                {activeItem && (
                  <div className="text-[10px] uppercase tracking-wider text-white/30 px-1">
                    Current
                  </div>
                )}
              </>
            )}

            {/* Active thinking at the bottom (sticky) */}
            {activeItem && (
              <ThinkingPanelItem item={activeItem} isActive={true} basePath={basePath} />
            )}
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
  summary: string | null;
} {
  if (!result) return { success: false, summary: null };

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
    return { success: !isError, summary };
  }

  // Handle object results
  if (typeof result === "object") {
    const resultObj = result as Record<string, unknown>;
    const statusLower = typeof resultObj.status === "string" ? resultObj.status.toLowerCase() : "";
    const isError = resultObj.error !== undefined ||
                    resultObj.is_error === true ||
                    resultObj.success === false ||
                    statusLower === "error" || statusLower === "failed";
    const summary = typeof resultObj.summary === "string" ? resultObj.summary :
                    typeof resultObj.message === "string" ? resultObj.message :
                    typeof resultObj.result === "string" ? truncateText(resultObj.result, 100) : null;
    return { success: !isError, summary };
  }

  return { success: true, summary: null };
}

// Subagent/Background Task tool item with enhanced UX
function SubagentToolItem({
  item,
}: {
  item: Extract<ChatItem, { kind: "tool" }>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const isDone = item.result !== undefined;

  const { agentName, description, prompt } = extractSubagentInfo(item.args);
  const { success, summary } = isDone ? parseSubagentResult(item.result) : { success: false, summary: null };

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

  const resultStr = item.result !== undefined ? formatToolArgs(item.result) : null;

  return (
    <div className="my-3">
      {/* Main card */}
      <div
        className={cn(
          "rounded-lg border overflow-hidden",
          "bg-white/[0.02]",
          !isDone && "border-purple-500/30",
          isDone && success && "border-emerald-500/20",
          isDone && !success && "border-red-500/20"
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
            isDone && success && "bg-emerald-500/20",
            isDone && !success && "bg-red-500/20"
          )}>
            {!isDone ? (
              <Cpu className="h-4 w-4 text-purple-400 animate-pulse" />
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
                isDone && success && "text-emerald-300",
                isDone && !success && "text-red-300"
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

        {/* Expandable content */}
        <div
          className={cn(
            "overflow-hidden transition-all duration-200 ease-out",
            expanded ? "max-h-[600px] opacity-100" : "max-h-0 opacity-0"
          )}
        >
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
                  <SyntaxHighlighter
                    language="json"
                    style={oneDark}
                    customStyle={{
                      margin: 0,
                      padding: "0.5rem",
                      fontSize: "0.75rem",
                      borderRadius: "0.25rem",
                      background: !success ? "rgba(239, 68, 68, 0.1)" : "rgba(0, 0, 0, 0.2)",
                    }}
                    codeTagProps={{
                      style: {
                        fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
                        color: !success ? "rgb(248, 113, 113)" : undefined,
                      },
                    }}
                  >
                    {resultStr}
                  </SyntaxHighlighter>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// Extract image file paths from tool result strings
// Matches patterns like "/path/to/image.png" or "screenshots/file.jpg"
function extractImagePaths(text: string): string[] {
  const paths: string[] = [];
  // Match absolute paths or relative paths that look like image files
  // Pattern: word characters, slashes, dots, hyphens, underscores ending in image extension
  const pathPattern = /(?:\/[\w\-._/]+|[\w\-._]+\/[\w\-._/]+)\.(?:png|jpg|jpeg|gif|webp|bmp|svg)\b/gi;
  const matches = text.match(pathPattern);
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
function ImagePreview({ path }: { path: string }) {
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
        const res = await fetch(
          `${API_BASE}/api/fs/download?path=${encodeURIComponent(path)}`,
          { headers: { ...authHeader() } }
        );
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
  }, [path]);

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
function ToolCallItem({
  item,
}: {
  item: Extract<ChatItem, { kind: "tool" }>;
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
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  // Use endTime for completed tools, otherwise use elapsed time for running tools
  const duration = isDone && item.endTime
    ? formatDuration(Math.floor((item.endTime - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  const argsStr = formatToolArgs(item.args);
  const resultStr = item.result !== undefined ? formatToolArgs(item.result) : null;
  
  // Determine result status - check for explicit error indicators, not just keyword presence
  const isError = (() => {
    if (resultStr === null) return false;

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
  })();

  // Get a preview of the args for the collapsed state
  const argsPreview = truncateText(
    typeof item.args === "object" && item.args !== null
      ? Object.keys(item.args as Record<string, unknown>).slice(0, 2).join(", ")
      : argsStr,
    50
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
          isDone && !isError && "border-emerald-500/20",
          isDone && isError && "border-red-500/20"
        )}
      >
        <ToolIcon
          className={cn(
            "h-3 w-3",
            !isDone && "animate-pulse text-amber-400",
            isDone && !isError && "text-emerald-400",
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
          {isDone ? `${duration}` : `${duration}...`}
        </span>
        {isDone && !isError && <CheckCircle className="h-3 w-3 text-emerald-400" />}
        {isDone && isError && <XCircle className="h-3 w-3 text-red-400" />}
        {!isDone && <Loader className="h-3 w-3 animate-spin text-amber-400" />}
        <ChevronDown
          className={cn(
            "h-3 w-3 transition-transform duration-200 ml-1",
            expanded ? "rotate-0" : "-rotate-90"
          )}
        />
      </button>

      {/* Expandable content with animation */}
      <div
        className={cn(
          "overflow-hidden transition-all duration-200 ease-out",
          expanded ? "max-h-[500px] opacity-100 mt-2" : "max-h-0 opacity-0"
        )}
      >
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3 space-y-3">
          {/* Arguments */}
          {argsStr && (
            <div>
              <div className="text-[10px] uppercase tracking-wider text-white/30 mb-1">
                Arguments
              </div>
              <div className="max-h-40 overflow-y-auto rounded">
                <SyntaxHighlighter
                  language="json"
                  style={oneDark}
                  customStyle={{
                    margin: 0,
                    padding: "0.5rem",
                    fontSize: "0.75rem",
                    borderRadius: "0.25rem",
                    background: "rgba(0, 0, 0, 0.2)",
                  }}
                  codeTagProps={{
                    style: {
                      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
                    },
                  }}
                >
                  {argsStr}
                </SyntaxHighlighter>
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
                <SyntaxHighlighter
                  language="json"
                  style={oneDark}
                  customStyle={{
                    margin: 0,
                    padding: "0.5rem",
                    fontSize: "0.75rem",
                    borderRadius: "0.25rem",
                    background: isError ? "rgba(239, 68, 68, 0.1)" : "rgba(0, 0, 0, 0.2)",
                  }}
                  codeTagProps={{
                    style: {
                      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
                      color: isError ? "rgb(248, 113, 113)" : undefined,
                    },
                  }}
                >
                  {resultStr}
                </SyntaxHighlighter>
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
                      <ImagePreview key={path} path={path} />
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
    </div>
  );
}

// Collapsed tool group component - shows last tool with expand option
function CollapsedToolGroup({
  tools,
  isExpanded,
  onToggleExpand,
}: {
  tools: Extract<ChatItem, { kind: "tool" }>[];
  isExpanded: boolean;
  onToggleExpand: () => void;
}) {
  const hiddenCount = tools.length - 1;
  const lastTool = tools[tools.length - 1];

  // Helper to render appropriate tool component
  const renderTool = (tool: Extract<ChatItem, { kind: "tool" }>) => {
    if (isSubagentTool(tool.name)) {
      return <SubagentToolItem key={tool.id} item={tool} />;
    }
    return <ToolCallItem key={tool.id} item={tool} />;
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
  const [lastMissionId, setLastMissionId] = useLocalStorage<string | null>(
    "control-last-mission-id",
    null
  );

  const [runState, setRunState] = useState<ControlRunState>("idle");
  const [queueLen, setQueueLen] = useState(0);

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

  // Progress state (for "Subtask X of Y" indicator)
  const [progress, setProgress] = useState<{
    total: number;
    completed: number;
    current: string | null;
    depth: number;
  } | null>(null);

  // Mission state
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [viewingMission, setViewingMission] = useState<Mission | null>(null);
  const [missionLoading, setMissionLoading] = useState(false);
  const [recentMissions, setRecentMissions] = useState<Mission[]>([]);

  // Workspaces for mission creation
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);

  // Library context for agents

  useEffect(() => {
    const interval = setInterval(() => setDiagTick((prev) => prev + 1), 1000);
    return () => clearInterval(interval);
  }, []);

  // Parallel missions state
  const [runningMissions, setRunningMissions] = useState<RunningMissionInfo[]>(
    []
  );
  const [showMissionSwitcher, setShowMissionSwitcher] = useState(false);

  // Track which mission's events we're viewing (for parallel missions)
  // This can differ from currentMission when viewing a parallel mission
  const [viewingMissionId, setViewingMissionId] = useState<string | null>(null);

  // Store items per mission to preserve context when switching
  const [missionItems, setMissionItems] = useState<Record<string, ChatItem[]>>(
    {}
  );

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

  // Provider and model selection state
  const [providers, setProviders] = useState<Provider[]>([]);

  // Server configuration (fetched from health endpoint)
  const [maxIterations, setMaxIterations] = useState<number>(50); // Default fallback

  // Desktop stream state
  const [showDesktopStream, setShowDesktopStream] = useState(false);
  const [desktopDisplayId, setDesktopDisplayId] = useState(":101");
  const [showDisplaySelector, setShowDisplaySelector] = useState(false);
  const [hasDesktopSession, setHasDesktopSession] = useState(false);
  const [desktopSessions, setDesktopSessions] = useState<DesktopSessionDetail[]>([]);
  const [isClosingDesktop, setIsClosingDesktop] = useState<string | null>(null);

  // Thinking panel state
  const [showThinkingPanel, setShowThinkingPanel] = useState(false);

  // Tool groups expansion state - tracks which groups are expanded by their first tool's id
  const [expandedToolGroups, setExpandedToolGroups] = useState<Set<string>>(new Set());

  // Extract thinking items for the side panel
  const thinkingItems = useMemo(() =>
    items.filter((it): it is Extract<ChatItem, { kind: "thinking" }> => it.kind === "thinking"),
    [items]
  );

  // Deduplicated count for display (same logic as ThinkingPanel)
  const thinkingItemsCount = useMemo(() => {
    const activeItem = thinkingItems.find(t => !t.done);
    const seen = new Set<string>();
    let completedCount = 0;
    for (const t of thinkingItems) {
      if (!t.done) continue;
      const trimmed = t.content.trim();
      if (!trimmed || seen.has(trimmed)) continue;
      seen.add(trimmed);
      completedCount++;
    }
    return completedCount + (activeItem ? 1 : 0);
  }, [thinkingItems]);

  // Check if there's active thinking happening
  const hasActiveThinking = useMemo(() =>
    thinkingItems.some(t => !t.done),
    [thinkingItems]
  );

  // Auto-show thinking panel when thinking starts (only on transition to active)
  const prevHasActiveThinking = useRef(false);
  useEffect(() => {
    // Only auto-show when transitioning from no active thinking to active thinking
    if (hasActiveThinking && !prevHasActiveThinking.current) {
      setShowThinkingPanel(true);
    }
    prevHasActiveThinking.current = hasActiveThinking;
  }, [hasActiveThinking]);

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
    thoughts: Extract<ChatItem, { kind: "thinking" }>[];
  };
  type GroupedItem = ChatItem | ToolGroup | ThinkingGroup;

  const groupedItems = useMemo((): GroupedItem[] => {
    const result: GroupedItem[] = [];
    let currentToolGroup: Extract<ChatItem, { kind: "tool" }>[] = [];
    let currentThinkingGroup: Extract<ChatItem, { kind: "thinking" }>[] = [];

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

    for (const item of items) {
      if (item.kind === "tool" && !item.isUiTool) {
        // Non-UI tool - flush thinking first, then add to tool group
        flushThinkingGroup();
        currentToolGroup.push(item);
      } else if (item.kind === "thinking") {
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
  }, [items, showThinkingPanel]);

  // Check if the mission we're viewing is actually running (not just any mission)
  const viewingMissionIsRunning = useMemo(() => {
    if (!viewingMissionId) return runState !== "idle";
    const mission = runningMissions.find((m) => m.mission_id === viewingMissionId);
    if (!mission) return false;
    // Check the actual state from the backend
    return mission.state === "running" || mission.state === "waiting_for_tool";
  }, [viewingMissionId, runningMissions, runState]);

  // Check if the mission we're viewing appears stalled (no activity for 60+ seconds)
  const viewingMissionStallSeconds = useMemo(() => {
    if (!viewingMissionId) return 0;
    const mission = runningMissions.find((m) => m.mission_id === viewingMissionId);
    if (!mission) return 0;
    if (mission.state !== "running") return 0;
    return mission.seconds_since_activity;
  }, [viewingMissionId, runningMissions]);

  const hasPendingQuestion = useMemo(
    () =>
      items.some(
        (item) =>
          item.kind === "tool" &&
          item.name === "question" &&
          item.result === undefined
      ),
    [items]
  );

  const isViewingMissionStalled = viewingMissionStallSeconds >= 60;
  const isViewingMissionSeverelyStalled = viewingMissionStallSeconds >= 120;

  const recentMissionList = useMemo(() => {
    if (recentMissions.length === 0) return [];
    const runningIds = new Set(runningMissions.map((m) => m.mission_id));
    const currentId = currentMission?.id ?? null;
    return recentMissions
      .filter((mission) => mission.id !== currentId && !runningIds.has(mission.id))
      .slice(0, 6);
  }, [recentMissions, runningMissions, currentMission?.id]);

  const isBusy = viewingMissionIsRunning;

  const streamCleanupRef = useRef<null | (() => void)>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const viewingMissionIdRef = useRef<string | null>(null);
  const runningMissionsRef = useRef<RunningMissionInfo[]>([]);
  const currentMissionRef = useRef<Mission | null>(null);
  const viewingMissionRef = useRef<Mission | null>(null);

  // Keep refs in sync with state
  useEffect(() => {
    viewingMissionIdRef.current = viewingMissionId;
  }, [viewingMissionId]);

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
  const { containerRef, endRef, isAtBottom, scrollToBottom } =
    useScrollToBottom();

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

  // Auto-resize textarea
  const adjustTextareaHeight = useCallback(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    textarea.style.height = "auto";
    const lineHeight = 20;
    const maxLines = 10;
    const maxHeight = lineHeight * maxLines;
    const newHeight = Math.min(textarea.scrollHeight, maxHeight);
    textarea.style.height = `${newHeight}px`;
  }, []);

  useEffect(() => {
    adjustTextareaHeight();
  }, [input, adjustTextareaHeight]);

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
  const handleFileUpload = useCallback(async (file: File) => {
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
      
      // Use chunked upload for files > 10MB, regular for smaller
      const useChunked = fileToUpload.size > 10 * 1024 * 1024;
      
      const result = useChunked 
        ? await uploadFileChunked(fileToUpload, contextPath, (progress) => {
            setUploadProgress({ fileName: displayName, progress });
          })
        : await uploadFile(fileToUpload, contextPath, (progress) => {
            setUploadProgress({ fileName: displayName, progress });
          });
      
      toast.success(`Uploaded ${result.name}`);

      // Add a message about the upload at the beginning
      setInput((prev) => {
        const uploadNote = `[Uploaded: ${result.name}]`;
        return prev ? `${uploadNote}\n${prev}` : uploadNote;
      });
    } catch (error) {
      console.error("Upload failed:", error);
      toast.error(`Failed to upload ${displayName}`);
    } finally {
      setUploadQueue((prev) => prev.filter((name) => name !== displayName));
      setUploadProgress(null);
    }
  }, [compressImageFile, currentMission?.id]);

  // Handle URL download
  const handleUrlDownload = useCallback(async () => {
    if (!urlInput.trim()) return;
    
    setUrlDownloading(true);
    try {
      const contextPath = "./context/";
      
      const result = await downloadFromUrl(urlInput.trim(), contextPath);
      toast.success(`Downloaded ${result.name}`);
      
      // Add a message about the download at the beginning (consistent with uploads)
      setInput((prev) => {
        const downloadNote = `[Downloaded: ${result.name}]`;
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
  }, [urlInput, currentMission?.id]);

  // Handle paste to upload files
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    const handlePaste = async (event: ClipboardEvent) => {
      const items = event.clipboardData?.items;
      if (!items) return;

      const files: File[] = [];
      for (const item of items) {
        if (item.kind === "file") {
          const file = item.getAsFile();
          if (file) files.push(file);
        }
      }

      if (files.length === 0) return;

      const textData = event.clipboardData?.getData("text/plain") ?? "";
      if (textData.trim().length > 0) {
        return;
      }

      // Prevent default paste for files
      event.preventDefault();

      // Upload files
      for (const file of files) {
        await handleFileUpload(file);
      }
    };

    textarea.addEventListener("paste", handlePaste);
    return () => textarea.removeEventListener("paste", handlePaste);
  }, [handleFileUpload]);

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
          entry.content.includes("desktop_desktop_start_session")
        ) {
          hasSession = true;
        }
        // Check for session close (must come after start check to handle same entry)
        if (
          entry.content.includes("desktop_close_session") ||
          entry.content.includes("desktop_desktop_close_session")
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
        setDesktopDisplayId(activeSession.display);
        setHasDesktopSession(true);
        return;
      }
      setHasDesktopSession(missionHasDesktopSession(mission));
    },
    [getActiveDesktopSession, missionHasDesktopSession]
  );

  // Derive working directory from mission's desktop sessions for file path resolution
  const missionWorkingDirectory = useMemo(() => {
    const mission = viewingMission ?? currentMission;
    if (!mission?.desktop_sessions?.length) return undefined;

    // Try to find screenshots_dir from any session (prefer active/latest)
    for (let i = mission.desktop_sessions.length - 1; i >= 0; i--) {
      const session = mission.desktop_sessions[i];
      if (session?.screenshots_dir) {
        // screenshots_dir is like /path/to/workspaces/mission-xxx/screenshots/
        // We want the parent: /path/to/workspaces/mission-xxx/
        const dir = session.screenshots_dir.replace(/\/?$/, ''); // remove trailing slash
        const parent = dir.substring(0, dir.lastIndexOf('/'));
        if (parent) return parent;
      }
    }
    return undefined;
  }, [viewingMission, currentMission]);

  const missionHistoryToItems = useCallback((mission: Mission): ChatItem[] => {
    // Estimate timestamps based on mission creation time
    const baseTime = new Date(mission.created_at).getTime();
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
        return {
          kind: "assistant" as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
          success: true,
          costCents: 0,
          model: null,
          timestamp,
        };
      }
    });
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
      const previousViewingMission = viewingMissionRef.current;
      setMissionLoading(true);
      setViewingMissionId(id); // Set viewing ID immediately to prevent "Agent is working..." flash
      fetchingMissionIdRef.current = id; // Track which mission we're loading
      try {
        const mission = await loadMission(id);
        if (cancelled || fetchingMissionIdRef.current !== id) return;
        setCurrentMission(mission);
        setViewingMission(mission);
        setItems(missionHistoryToItems(mission));
        applyDesktopSessionState(mission);
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
          applyDesktopSessionState(fallbackMission);
        } else {
          setViewingMissionId(null);
          setViewingMission(null);
          setItems([]);
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
          setItems(missionHistoryToItems(mission));
          applyDesktopSessionState(mission);
          router.replace(`/control?mission=${mission.id}`, { scroll: false });
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
    applyDesktopSessionState,
    authRetryTrigger,
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
      // Update hasDesktopSession based on whether there are any running sessions
      const hasRunning = sessions.some(s => s.process_running && s.status !== 'stopped');
      if (hasRunning && !hasDesktopSession) {
        setHasDesktopSession(true);
      }
    } catch (err) {
      if (isNetworkError(err)) return;
      // Silently fail - desktop sessions are optional
    }
  }, [hasDesktopSession]);

  useEffect(() => {
    refreshDesktopSessions();
    const interval = setInterval(refreshDesktopSessions, 10000);
    return () => clearInterval(interval);
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

  // Fetch available providers and models for mission creation (retry on auth success)
  useEffect(() => {
    listProviders()
      .then((data) => {
        setProviders(data.providers);
      })
      .catch((err) => {
        if (isNetworkError(err)) return;
        console.error("Failed to fetch providers:", err);
      });
  }, [authRetryTrigger]);

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

  // Handle switching which mission we're viewing
  const handleViewMission = useCallback(
    async (missionId: string) => {
      const previousViewingId = viewingMissionIdRef.current;
      const previousViewingMission = viewingMissionRef.current;

      setViewingMissionId(missionId);
      fetchingMissionIdRef.current = missionId;

      // Update URL immediately so it's shareable/bookmarkable
      router.replace(`/control?mission=${missionId}`, { scroll: false });

      // Always load fresh history from API when switching missions
      // This ensures we don't show stale cached events
      try {
        const mission = await getMission(missionId);

        // Race condition guard: only update if this is still the mission we want
        if (fetchingMissionIdRef.current !== missionId) {
          return; // Another mission was requested, discard this response
        }

        const historyItems = missionHistoryToItems(mission);
        setItems(historyItems);
        // Check if mission has an active desktop session (stored metadata or fallback to history)
        applyDesktopSessionState(mission);
        // Update cache with fresh data
        setMissionItems((prev) => ({ ...prev, [missionId]: historyItems }));
        setViewingMission(mission);
        if (currentMissionRef.current?.id === mission.id) {
          setCurrentMission(mission);
        }
      } catch (err) {
        console.error("Failed to load mission:", err);

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
          applyDesktopSessionState(fallbackMission);
          router.replace(`/control?mission=${fallbackMission.id}`, { scroll: false });
        } else if (previousViewingId && missionItems[previousViewingId]) {
          setViewingMissionId(previousViewingId);
          setViewingMission(null);
          setItems(missionItems[previousViewingId]);
          router.replace(`/control?mission=${previousViewingId}`, { scroll: false });
        } else {
          setViewingMissionId(null);
          setViewingMission(null);
          setItems([]);
          setHasDesktopSession(false);
          router.replace(`/control`, { scroll: false });
        }
      }
    },
    [missionItems, missionHistoryToItems, applyDesktopSessionState, router]
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
  const handleNewMission = async (options?: {
    workspaceId?: string;
    agent?: string;
    modelOverride?: string;
  }) => {
    try {
      setMissionLoading(true);
      const mission = await createMission({
        workspaceId: options?.workspaceId,
        agent: options?.agent,
        modelOverride: options?.modelOverride,
      });
      pendingMissionNavRef.current = mission.id;
      router.replace(`/control?mission=${mission.id}`, { scroll: false });
      setCurrentMission(mission);
      setViewingMission(mission);
      setViewingMissionId(mission.id); // Also update viewing to the new mission
      setItems([]);
      setHasDesktopSession(false);
      // Refresh running missions to get accurate state
      const running = await getRunningMissions();
      setRunningMissions(running);
      refreshRecentMissions();
      toast.success("New mission created");
    } catch (err) {
      console.error("Failed to create mission:", err);
      toast.error("Failed to create new mission");
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
  const handleResumeMission = async (cleanWorkspace: boolean = false) => {
    const mission = viewingMission ?? currentMission;
    if (!mission || !["interrupted", "blocked"].includes(mission.status)) return;
    try {
      setMissionLoading(true);
      const resumed = await resumeMission(mission.id, cleanWorkspace);
      setCurrentMission(resumed);
      setViewingMission(resumed);
      setViewingMissionId(resumed.id);
      const historyItems = missionHistoryToItems(resumed);
      setItems(historyItems);
      setMissionItems((prev) => ({ ...prev, [resumed.id]: historyItems }));
      refreshRecentMissions();
      toast.success(
        cleanWorkspace 
          ? "Mission resumed with clean workspace" 
          : (mission.status === "blocked" ? "Continuing mission" : "Mission resumed")
      );
    } catch (err) {
      console.error("Failed to resume mission:", err);
      toast.error("Failed to resume mission");
    } finally {
      setMissionLoading(false);
    }
  };

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
          setProgress({
            total: p.total_subtasks,
            completed: p.completed_subtasks,
            current: p.current_subtask,
            depth: p.current_depth,
          });
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
          getMission(viewingId)
            .then((mission) => {
              if (!mounted) return;
              const historyItems = missionHistoryToItems(mission);
              setItems(historyItems);
              setMissionItems((prev) => ({ ...prev, [viewingId]: historyItems }));
            })
            .catch(() => {}); // Ignore errors - we'll get updates via stream
        }

        const st = data["state"];
        const newState =
          typeof st === "string" ? (st as ControlRunState) : "idle";
        const q = data["queue_len"];

        // Status filtering: only apply status if it matches the mission we're viewing
        const statusMissionId = typeof data["mission_id"] === "string" ? data["mission_id"] : null;
        let shouldApplyStatus = true;

        if (statusMissionId) {
          // Status for a specific mission - only apply if viewing that mission
          shouldApplyStatus = statusMissionId === viewingId;
        } else {
          // Status for main session - only apply if viewing main mission, no specific mission,
          // or currentMissionId hasn't loaded yet (to match event filter logic and avoid
          // desktop stream staying open when status=idle comes during loading)
          shouldApplyStatus = !viewingId || viewingId === currentMissionId || !currentMissionId;
        }

        if (shouldApplyStatus) {
          setQueueLen(typeof q === "number" ? q : 0);

          // Clear progress and auto-close desktop stream when idle
          if (newState === "idle") {
            setProgress(null);
            // Auto-close desktop stream when agent finishes
            setShowDesktopStream(false);
          }

          // If we reconnected and agent is already running, add a visual indicator
          setRunState((prevState) => {
            // Only show reconnect notice if we weren't already tracking this as running
            // and there's no active thinking/phase item (means we missed some events)
            if (newState === "running" && prevState === "idle") {
              setItems((prevItems) => {
                const hasActiveThinking = prevItems.some(
                  (it) =>
                    (it.kind === "thinking" && !it.done) || it.kind === "phase"
                );
                // If there's no active streaming item, the user is seeing stale state
                // The "Agent is working..." indicator will show via the render logic
                return prevItems;
              });
            }
            return newState;
          });
        }
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

          // No matching temp message found, add new (message came from another client/session)
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
        setItems((prev) => [
          ...prev.filter((it) => it.kind !== "thinking" || it.done),
          {
            kind: "assistant",
            id: String(data["id"] ?? Date.now()),
            content: String(data["content"] ?? ""),
            success: Boolean(data["success"]),
            costCents: Number(data["cost_cents"] ?? 0),
            model: data["model"] ? String(data["model"]) : null,
            timestamp: Date.now(),
            sharedFiles,
            resumable,
          },
        ]);
        return;
      }

      if (event.type === "thinking" && isRecord(data)) {
        const content = String(data["content"] ?? "");
        const done = Boolean(data["done"]);
        const now = Date.now();

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

            // If this is a done marker or content update for the same thought, update in place
            if (done || !content || existing.content.startsWith(content) || content.startsWith(existing.content)) {
              updated[existingIdx] = {
                ...existing,
                // When done marker arrives with empty content, preserve existing content
                // Backend sends cumulative content normally, but final done marker may be empty
                content: content || existing.content,
                done,
                // Set endTime when marking as done
                ...(done && { endTime: now }),
              };
              return updated;
            }

            // New thought content that doesn't match existing - mark existing as done and create new
            updated[existingIdx] = {
              ...existing,
              done: true,
              endTime: now,
            };
            return [
              ...updated,
              {
                kind: "thinking" as const,
                id: `thinking-${now}`,
                content,
                done: false,
                startTime: now,
              },
            ];
          } else {
            return [
              ...filtered,
              {
                kind: "thinking" as const,
                id: `thinking-${now}`,
                content,
                done,
                startTime: now,
                // Set endTime if creating a pre-done item (unlikely but handle it)
                ...(done && { endTime: now }),
              },
            ];
          }
        });
        return;
      }

      if (event.type === "tool_call" && isRecord(data)) {
        const name = String(data["name"] ?? "");
        const isUiTool = name.startsWith("ui_") || name === "question";

        setItems((prev) => [
          ...prev,
          {
            kind: "tool",
            id: `tool-${String(data["tool_call_id"] ?? Date.now())}`,
            toolCallId: String(data["tool_call_id"] ?? ""),
            name,
            args: data["args"],
            isUiTool,
            startTime: Date.now(),
          },
        ]);
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
        if (eventToolName === "desktop_start_session" || eventToolName === "desktop_desktop_start_session") {
          let result = data["result"];
          // Handle case where result is a JSON string that needs parsing
          if (typeof result === "string") {
            try {
              result = JSON.parse(result);
            } catch {
              // Not valid JSON, leave as-is
            }
          }
          if (isRecord(result) && typeof result["display"] === "string") {
            const display = result["display"];
            setDesktopDisplayId(display);
            setHasDesktopSession(true);
            // Auto-open desktop stream when session starts
            setShowDesktopStream(true);
          }
        }
        // Handle desktop session close
        if (eventToolName === "desktop_close_session" || eventToolName === "desktop_desktop_close_session") {
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
            if (toolName === "desktop_start_session" || toolName === "desktop_desktop_start_session") {
              let result = data["result"];
              // Handle case where result is a JSON string that needs parsing
              if (typeof result === "string") {
                try {
                  result = JSON.parse(result);
                } catch {
                  // Not valid JSON, leave as-is
                }
              }
              if (isRecord(result) && typeof result["display"] === "string") {
                const display = result["display"];
                setDesktopDisplayId(display);
                setHasDesktopSession(true);
                setShowDesktopStream(true);
              }
            }
            // Check for desktop_close_session
            if (toolName === "desktop_close_session" || toolName === "desktop_desktop_close_session") {
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

      // Handle progress updates
      if (event.type === "progress" && isRecord(data)) {
        setProgress({
          total: Number(data["total_subtasks"] ?? 0),
          completed: Number(data["completed_subtasks"] ?? 0),
          current: data["current_subtask"] as string | null,
          depth: Number(data["depth"] ?? 0),
        });
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
    };
  }, []);

  const status = useMemo(() => statusLabel(runState), [runState]);
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
    const payload = {
      captured_at: new Date().toISOString(),
      connection_state: connectionState,
      reconnect_attempt: reconnectAttempt,
      stream: streamDiagnostics,
      user_agent: typeof navigator !== "undefined" ? navigator.userAgent : undefined,
    };
    try {
      await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
      toast.success("Copied stream diagnostics");
    } catch {
      toast.error("Failed to copy diagnostics");
    }
  }, [connectionState, reconnectAttempt, streamDiagnostics]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const content = input.trim();
    if (!content) return;

    const targetMissionId = viewingMissionIdRef.current;
    const currentMissionId = currentMissionRef.current?.id ?? null;

    if (targetMissionId && targetMissionId !== currentMissionId) {
      try {
        console.debug("[control] switching current mission before send", {
          from: currentMissionId,
          to: targetMissionId,
        });
        const mission = await loadMission(targetMissionId);
        setCurrentMission(mission);
        setViewingMission(mission);
        setViewingMissionId(mission.id);
        setItems(missionHistoryToItems(mission));
        applyDesktopSessionState(mission);
      } catch (err) {
        console.error("Failed to switch mission before sending:", err);
        toast.error("Failed to switch mission before sending");
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
    const willBeQueued = runState !== "idle" && hasExistingUserMessages;

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

    const targetMissionId = viewingMissionIdRef.current;
    const currentMissionId = currentMissionRef.current?.id ?? null;

    if (targetMissionId && targetMissionId !== currentMissionId) {
      try {
        console.debug("[control] switching current mission before send", {
          from: currentMissionId,
          to: targetMissionId,
        });
        const mission = await loadMission(targetMissionId);
        setCurrentMission(mission);
        setViewingMission(mission);
        setViewingMissionId(mission.id);
        setItems(missionHistoryToItems(mission));
        applyDesktopSessionState(mission);
      } catch (err) {
        console.error("Failed to switch mission before sending:", err);
        toast.error("Failed to switch mission before sending");
        return;
      }
    }

    setInput("");
    setDraftInput("");

    const tempId = `temp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const timestamp = Date.now();
    const hasExistingUserMessages = items.some((item) => item.kind === "user");
    const willBeQueued = runState !== "idle" && hasExistingUserMessages;

    // Display the original content with agent mention for UI
    const displayContent = agent ? `@${agent} ${content}` : content;

    setItems((prev) => [
      ...prev,
      {
        kind: "user" as const,
        id: tempId,
        content: displayContent,
        timestamp,
        queued: willBeQueued,
      },
    ]);

    try {
      // Send content to API, with agent as separate parameter
      const { id, queued } = await postControlMessage(content, agent ? { agent } : undefined);
      setItems((prev) => {
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
    }
  }, [items, runState, applyDesktopSessionState, missionHistoryToItems]);

  const handleStop = async () => {
    try {
      await cancelControl();
      toast.success("Cancelled");
    } catch (err) {
      console.error(err);
      toast.error("Failed to cancel");
    }
  };

  const activeMission = viewingMission ?? currentMission;
  const missionStatus = activeMission
    ? missionStatusLabel(activeMission.status)
    : null;

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
        onSelectMission={(missionId) => {
          handleViewMission(missionId);
        }}
        onCancelMission={handleCancelMission}
        onRefresh={refreshRecentMissions}
      />

      {/* Header */}
      <div className="mb-6 flex items-center justify-between gap-4">
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
                  <span className="text-sm font-medium text-white/70">
                    {activeMission.workspace_name || activeMission.id.slice(0, 8)}
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
            providers={providers}
            disabled={missionLoading}
            onCreate={handleNewMission}
          />

          {/* Thinking panel toggle */}
          <button
            onClick={() => setShowThinkingPanel(!showThinkingPanel)}
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
                        {desktopSessions.filter(s => s.process_running).map((session) => (
                          <div
                            key={session.display}
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
                                session.status === 'active' ? "bg-emerald-500" :
                                session.status === 'orphaned' ? "bg-amber-500" :
                                "bg-gray-500"
                              )} title={session.status} />

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
                                session.status === 'active' ? "text-emerald-500/70" :
                                session.status === 'orphaned' ? "text-amber-500/70" :
                                "text-white/40"
                              )}>
                                {session.status === 'active' ? 'Active' :
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

            {/* Stream diagnostics toggle */}
            <div className="relative">
              <button
                onClick={() => setShowStreamDiagnostics((prev) => !prev)}
                className="flex items-center gap-2 rounded-md border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-xs text-white/70 transition-colors hover:bg-white/[0.04]"
                title="Stream diagnostics"
              >
                <span
                  className={cn(
                    "h-2 w-2 rounded-full",
                    (streamDiagnostics.phase === "streaming" || streamDiagnostics.phase === "open") &&
                      "bg-emerald-400",
                    streamDiagnostics.phase === "connecting" && "bg-amber-400 animate-pulse",
                    streamDiagnostics.phase === "error" && "bg-red-400",
                    streamDiagnostics.phase === "closed" && "bg-white/30",
                    streamDiagnostics.phase === "idle" && "bg-white/20"
                  )}
                />
                Stream
                <ChevronDown className="h-3 w-3 text-white/50" />
              </button>

              {showStreamDiagnostics && (
                <div className="absolute right-0 top-full z-50 mt-2 w-[360px] rounded-lg border border-white/[0.08] bg-[#121214] p-3 shadow-xl">
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-medium text-white/80">Stream diagnostics</span>
                    <button
                      onClick={handleCopyDiagnostics}
                      className="text-[11px] text-white/50 hover:text-white/80 transition-colors"
                    >
                      Copy
                    </button>
                  </div>

                  <div className="mt-2 space-y-1 text-xs text-white/70">
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Phase</span>
                      <span className="font-mono text-white/80">{streamDiagnostics.phase}</span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">URL</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {streamDiagnostics.url ?? "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Status</span>
                      <span className="font-mono text-white/80">
                        {typeof streamDiagnostics.status === "number"
                          ? streamDiagnostics.status
                          : "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Content-Type</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {streamDiagnostics.contentType ?? "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Content-Encoding</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {streamDiagnostics.contentEncoding ?? "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Cache-Control</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {streamDiagnostics.cacheControl ?? "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Transfer-Encoding</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {streamDiagnostics.transferEncoding ?? "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Server/Via</span>
                      <span className="max-w-[230px] truncate font-mono text-white/80">
                        {[streamDiagnostics.server, streamDiagnostics.via]
                          .filter(Boolean)
                          .join(" • ") || "—"}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Last event</span>
                      <span className="font-mono text-white/80">
                        {formatDiagAge(streamDiagnostics.lastEventAt)}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Last chunk</span>
                      <span className="font-mono text-white/80">
                        {formatDiagAge(streamDiagnostics.lastChunkAt)}
                      </span>
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-white/40">Bytes</span>
                      <span className="font-mono text-white/80">
                        {streamDiagnostics.bytes.toLocaleString()}
                      </span>
                    </div>
                  </div>

                  {streamDiagnostics.lastError && (
                    <div className="mt-2 rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1 text-xs text-red-300">
                      {streamDiagnostics.lastError}
                    </div>
                  )}

                  {streamHints.length > 0 && (
                    <div className="mt-2 space-y-1 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-200">
                      {streamHints.map((hint) => (
                        <div key={hint}>{hint}</div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>

            <div className="h-4 w-px bg-white/[0.08]" />

            {/* Run state indicator */}
            <div className={cn("flex items-center gap-2", status.className)}>
              <StatusIcon
                className={cn(
                  "h-3.5 w-3.5",
                  runState !== "idle" && "animate-spin"
                )}
              />
              <span className="text-sm font-medium">{status.label}</span>
            </div>

            {/* Queue count */}
            <div className="h-4 w-px bg-white/[0.08]" />
            <div
              className="flex items-center gap-1.5"
              title={queueLen > 0 ? `${queueLen} message${queueLen > 1 ? 's' : ''} waiting to be processed` : 'No messages queued'}
            >
              <span className="text-[10px] uppercase tracking-wider text-white/40">
                Queue
              </span>
              <span className={cn(
                "text-sm font-medium tabular-nums",
                queueLen === 0 && "text-white/70",
                queueLen > 0 && queueLen < 3 && "text-amber-400",
                queueLen >= 3 && "text-orange-400"
              )}>
                {queueLen}
              </span>
            </div>

            {/* Progress indicator */}
            {progress && progress.total > 0 && (
              <>
                <div className="h-4 w-px bg-white/[0.08]" />
                <div className="flex items-center gap-1.5">
                  <span className="text-[10px] uppercase tracking-wider text-white/40">
                    Subtask
                  </span>
                  <span className="text-sm font-medium text-emerald-400 tabular-nums">
                    {progress.completed + 1}/{progress.total}
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
                          onClick={() => handleResumeMission(false)}
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
                        <button
                          onClick={() => handleResumeMission(true)}
                          disabled={missionLoading}
                          className="inline-flex items-center gap-2 rounded-lg bg-white/10 border border-white/20 px-4 py-2 text-sm font-medium text-white/70 hover:bg-white/20 hover:text-white transition-colors disabled:opacity-50"
                          title="Delete work folder and start fresh"
                        >
                          <Trash2 className="h-4 w-4" />
                          Clean & Continue
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
              {groupedItems.map((item) => {
                // Handle tool groups (multiple consecutive tools collapsed)
                if (item.kind === "tool_group") {
                  const isExpanded = expandedToolGroups.has(item.groupId);
                  return (
                    <CollapsedToolGroup
                      key={item.groupId}
                      tools={item.tools}
                      isExpanded={isExpanded}
                      onToggleExpand={() => {
                        setExpandedToolGroups((prev) => {
                          const next = new Set(prev);
                          if (next.has(item.groupId)) {
                            next.delete(item.groupId);
                          } else {
                            next.add(item.groupId);
                          }
                          return next;
                        });
                      }}
                    />
                  );
                }

                if (item.kind === "user") {
                  const isQueued = item.queued === true;
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
                            isQueued
                              ? "border-2 border-dashed border-indigo-500/60 bg-indigo-500/20"
                              : "bg-indigo-500"
                          )}
                        >
                          <p className="whitespace-pre-wrap text-sm">
                            {item.content}
                          </p>
                        </div>
                        <div className="mt-1 text-right flex items-center justify-end gap-2">
                          {isQueued && (
                            <span className="text-[10px] text-white/30">
                              Queued
                            </span>
                          )}
                          <span className="text-[10px] text-white/30">
                            {formatTime(item.timestamp)}
                          </span>
                        </div>
                      </div>
                      <div className={cn(
                        "flex h-8 w-8 shrink-0 items-center justify-center rounded-full",
                        isQueued ? "bg-white/[0.04]" : "bg-white/[0.08]"
                      )}>
                        <User className={cn("h-4 w-4", isQueued ? "text-white/40" : "text-white/60")} />
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
                          {item.costCents > 0 && (
                            <>
                              <span>•</span>
                              <span className="text-emerald-400">
                                ${(item.costCents / 100).toFixed(4)}
                              </span>
                            </>
                          )}
                          <span>•</span>
                          <span className="text-white/30">
                            {formatTime(item.timestamp)}
                          </span>
                        </div>
                        <MarkdownContent content={item.content} basePath={missionWorkingDirectory} />
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
                              onClick={() => handleResumeMission(false)}
                              className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                            >
                              <RotateCcw className="h-3 w-3" />
                              Resume Mission
                            </button>
                            <button
                              onClick={() => handleResumeMission(true)}
                              className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white/60 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors"
                              title="Resume and clean workspace"
                            >
                              <RefreshCw className="h-3 w-3" />
                              Clean Resume
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
                  return <ThinkingGroupItem key={item.groupId} items={item.thoughts} basePath={missionWorkingDirectory} />;
                }

                if (item.kind === "thinking") {
                  // Fallback for individual thinking items (should be rare with grouping)
                  return <ThinkingGroupItem key={item.id} items={[item]} basePath={missionWorkingDirectory} />;
                }

                if (item.kind === "tool") {
                  // UI tools get special interactive rendering
                  if (item.isUiTool) {
                    if (item.name === "question") {
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
                    return <ToolCallItem key={item.id} item={item} />;
                  }

                  // Subagent/background task tools get enhanced rendering
                  if (isSubagentTool(item.name)) {
                    return <SubagentToolItem key={item.id} item={item} />;
                  }

                  // Non-UI tools use the collapsible ToolCallItem component
                  return <ToolCallItem key={item.id} item={item} />;
                }

                // system
                return (
                  <div key={item.id} className="flex justify-start gap-3">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.04]">
                      <Ban className="h-4 w-4 text-white/40" />
                    </div>
                    <div className="max-w-[80%] rounded-2xl rounded-tl-md bg-white/[0.02] border border-white/[0.04] px-4 py-3">
                      <p className="whitespace-pre-wrap text-sm text-white/60">
                        {item.content}
                      </p>
                      {item.resumable && (
                        <div className="mt-3 flex gap-2">
                          <button
                            onClick={() => handleResumeMission(false)}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                          >
                            <RotateCcw className="h-3 w-3" />
                            Resume Mission
                          </button>
                          <button
                            onClick={() => handleResumeMission(true)}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white/60 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors"
                            title="Resume and clean workspace"
                          >
                            <RefreshCw className="h-3 w-3" />
                            Clean Resume
                          </button>
                        </div>
                      )}
                    </div>
                  </div>
                );
              })}

              {/* Show streaming indicator when running but no active thinking/phase */}
              {viewingMissionIsRunning &&
                items.length > 0 &&
                !items.some(
                  (it) =>
                    (it.kind === "thinking" && !it.done) || it.kind === "phase"
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
                      onClick={() => handleResumeMission(false)}
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
                    <button
                      onClick={() => handleResumeMission(true)}
                      disabled={missionLoading}
                      className="inline-flex items-center gap-1.5 rounded-lg bg-white/10 border border-white/20 px-3 py-1.5 text-sm font-medium text-white/70 hover:bg-white/20 hover:text-white transition-colors disabled:opacity-50"
                      title="Delete work folder and start fresh"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      Clean & Continue
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
          {activeMission && (activeMission.status === 'interrupted' || activeMission.status === 'blocked') ? (
            <div className="mx-auto flex max-w-3xl gap-3 items-center justify-center py-2">
              <div className="flex items-center gap-2 text-sm text-white/50 mr-4">
                <AlertTriangle className="h-4 w-4 text-amber-400" />
                <span>Mission {activeMission.status === 'blocked' ? 'blocked' : 'interrupted'}</span>
              </div>
              <button
                onClick={() => handleResumeMission(false)}
                disabled={missionLoading}
                className="flex items-center gap-2 rounded-xl bg-emerald-500 hover:bg-emerald-600 px-5 py-3 text-sm font-medium text-white transition-colors disabled:opacity-50"
              >
                <PlayCircle className="h-4 w-4" />
                {activeMission.status === 'blocked' ? 'Continue' : 'Resume'}
              </button>
              <button
                onClick={() => handleResumeMission(true)}
                disabled={missionLoading}
                className="flex items-center gap-2 rounded-xl border border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04] px-5 py-3 text-sm font-medium text-white/70 transition-colors disabled:opacity-50"
                title="Delete work folder and start fresh"
              >
                <Trash2 className="h-4 w-4 text-orange-400" />
                Clean & {activeMission.status === 'blocked' ? 'Continue' : 'Resume'}
              </button>
            </div>
          ) : (
            <form
              onSubmit={(e) => e.preventDefault()}
              className="mx-auto flex max-w-3xl gap-3 items-end"
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
                value={input}
                onChange={setInput}
                onSubmit={handleEnhancedSubmit}
                placeholder="Message the root agent… (paste files to upload)"
              />

              {isBusy ? (
                <button
                  type="button"
                  onClick={handleStop}
                  className="flex items-center gap-2 rounded-xl bg-red-500 hover:bg-red-600 px-5 py-3 text-sm font-medium text-white transition-colors shrink-0"
                >
                  <Square className="h-4 w-4" />
                  Stop
                </button>
              ) : (
                <button
                  type="submit"
                  disabled={!input.trim()}
                  className="flex items-center gap-2 rounded-xl bg-indigo-500 hover:bg-indigo-600 px-5 py-3 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
                >
                  <Send className="h-4 w-4" />
                  Send
                </button>
              )}
            </form>
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
                onClose={() => setShowThinkingPanel(false)}
                className={showDesktopStream ? "flex-shrink-0 max-h-[40%]" : "flex-1"}
                basePath={missionWorkingDirectory}
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
