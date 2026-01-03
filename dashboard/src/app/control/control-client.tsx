"use client";

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { toast } from "sonner";
import { MarkdownContent } from "@/components/markdown-content";
import { cn } from "@/lib/utils";
import {
  cancelControl,
  postControlMessage,
  postControlToolResult,
  streamControl,
  loadMission,
  getMission,
  createMission,
  setMissionStatus,
  resumeMission,
  getCurrentMission,
  uploadFile,
  uploadFileChunked,
  downloadFromUrl,
  formatBytes,
  getProgress,
  getRunningMissions,
  cancelMission,
  listProviders,
  getHealth,
  type ControlRunState,
  type Mission,
  type MissionStatus,
  type RunningMissionInfo,
  type UploadProgress,
  type Provider,
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
  Plus,
  ChevronDown,
  ChevronRight,
  Target,
  Brain,
  Copy,
  Check,
  Paperclip,
  ArrowDown,
  Cpu,
  Layers,
  RefreshCw,
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
  PanelRightClose,
  PanelRight,
  Wifi,
  WifiOff,
} from "lucide-react";
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

type ChatItem =
  | {
      kind: "user";
      id: string;
      content: string;
      timestamp: number;
    }
  | {
      kind: "assistant";
      id: string;
      content: string;
      success: boolean;
      costCents: number;
      model: string | null;
      timestamp: number;
    }
  | {
      kind: "thinking";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
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
    }
  | {
      kind: "phase";
      id: string;
      phase: string;
      detail: string | null;
      agent: string | null;
    };

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

// Thinking item component with collapsible UI and auto-collapse
function ThinkingItem({
  item,
}: {
  item: Extract<ChatItem, { kind: "thinking" }>;
}) {
  const [expanded, setExpanded] = useState(!item.done); // Auto-expand while thinking
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const hasAutoCollapsedRef = useRef(false);

  // Update elapsed time while thinking is active
  useEffect(() => {
    if (item.done) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [item.done, item.startTime]);

  // Auto-collapse when thinking is done (with delay)
  // Longer delay for extended thinking sessions to let user see the summary
  useEffect(() => {
    if (item.done && expanded && !hasAutoCollapsedRef.current) {
      const duration = Math.floor((Date.now() - item.startTime) / 1000);
      // Don't auto-collapse if thinking was > 30 seconds (user may want to review)
      if (duration > 30) {
        hasAutoCollapsedRef.current = true; // Mark as handled but don't collapse
        return;
      }
      const timer = setTimeout(() => {
        setExpanded(false);
        hasAutoCollapsedRef.current = true;
      }, 1500); // Longer delay
      return () => clearTimeout(timer);
    }
  }, [item.done, expanded, item.startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
  };

  const duration = item.done
    ? formatDuration(Math.floor((Date.now() - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

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
            !item.done && "animate-pulse text-indigo-400"
          )}
        />
        <span className="text-xs">
          {item.done ? `Thought for ${duration}` : `Thinking for ${duration}`}
        </span>
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
          <div className="text-xs text-white/50 whitespace-pre-wrap overflow-y-auto max-h-[45vh] leading-relaxed">
            {item.content || <span className="italic text-white/30">Processing...</span>}
          </div>
        </div>
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
  
  // Determine result status
  const isError = resultStr !== null && (
    resultStr.toLowerCase().includes("error") ||
    resultStr.toLowerCase().includes("failed") ||
    resultStr.toLowerCase().includes("exception")
  );

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
              <pre className="text-xs text-white/50 whitespace-pre-wrap overflow-x-auto max-h-40 overflow-y-auto bg-black/20 rounded p-2 font-mono">
                {argsStr}
              </pre>
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
              <pre className={cn(
                "text-xs whitespace-pre-wrap overflow-x-auto max-h-40 overflow-y-auto rounded p-2 font-mono",
                isError ? "text-red-400/80 bg-red-500/10" : "text-white/50 bg-black/20"
              )}>
                {resultStr}
              </pre>
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
  const [draftInput, setDraftInput] = useLocalStorage("control-draft", "");
  const [input, setInput] = useState(draftInput);

  const [runState, setRunState] = useState<ControlRunState>("idle");
  const [queueLen, setQueueLen] = useState(0);

  // Connection state for SSE stream - starts as disconnected until first event received
  const [connectionState, setConnectionState] = useState<
    "connected" | "disconnected" | "reconnecting"
  >("disconnected");
  const [reconnectAttempt, setReconnectAttempt] = useState(0);

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
  const [showStatusMenu, setShowStatusMenu] = useState(false);
  const [missionLoading, setMissionLoading] = useState(false);

  // New mission dialog state
  const [showNewMissionDialog, setShowNewMissionDialog] = useState(false);
  const [newMissionModel, setNewMissionModel] = useState("");
  const newMissionDialogRef = useRef<HTMLDivElement>(null);

  // Parallel missions state
  const [runningMissions, setRunningMissions] = useState<RunningMissionInfo[]>(
    []
  );
  const [showParallelPanel, setShowParallelPanel] = useState(false);

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

  // Check if the mission we're viewing is actually running (not just any mission)
  const viewingMissionIsRunning = useMemo(() => {
    if (!viewingMissionId) return runState !== "idle";
    const mission = runningMissions.find((m) => m.mission_id === viewingMissionId);
    if (!mission) return false;
    // Check the actual state from the backend
    return mission.state === "running" || mission.state === "waiting_for_tool";
  }, [viewingMissionId, runningMissions, runState]);

  const isBusy = viewingMissionIsRunning;

  const streamCleanupRef = useRef<null | (() => void)>(null);
  const statusMenuRef = useRef<HTMLDivElement>(null);
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

  // Close status menu when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        statusMenuRef.current &&
        !statusMenuRef.current.contains(event.target as Node)
      ) {
        setShowStatusMenu(false);
      }
      if (
        newMissionDialogRef.current &&
        !newMissionDialogRef.current.contains(event.target as Node)
      ) {
        setShowNewMissionDialog(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  // Handle file upload - wrapped in useCallback to avoid stale closures
  const handleFileUpload = useCallback(async (file: File) => {
    setUploadQueue((prev) => [...prev, file.name]);
    setUploadProgress({ fileName: file.name, progress: { loaded: 0, total: file.size, percentage: 0 } });

    try {
      // Upload to mission-specific context folder if we have a mission
      const contextPath = currentMission?.id 
        ? `/root/context/${currentMission.id}/`
        : "/root/context/";
      
      // Use chunked upload for files > 10MB, regular for smaller
      const useChunked = file.size > 10 * 1024 * 1024;
      
      const result = useChunked 
        ? await uploadFileChunked(file, contextPath, (progress) => {
            setUploadProgress({ fileName: file.name, progress });
          })
        : await uploadFile(file, contextPath, (progress) => {
            setUploadProgress({ fileName: file.name, progress });
          });
      
      toast.success(`Uploaded ${result.name}`);

      // Add a message about the upload
      setInput((prev) => {
        const uploadNote = `[Uploaded: ${result.name}]`;
        return prev ? `${prev}\n${uploadNote}` : uploadNote;
      });
    } catch (error) {
      console.error("Upload failed:", error);
      toast.error(`Failed to upload ${file.name}`);
    } finally {
      setUploadQueue((prev) => prev.filter((name) => name !== file.name));
      setUploadProgress(null);
    }
  }, [currentMission?.id]);

  // Handle URL download
  const handleUrlDownload = useCallback(async () => {
    if (!urlInput.trim()) return;
    
    setUrlDownloading(true);
    try {
      const contextPath = currentMission?.id 
        ? `/root/context/${currentMission.id}/`
        : "/root/context/";
      
      const result = await downloadFromUrl(urlInput.trim(), contextPath);
      toast.success(`Downloaded ${result.name}`);
      
      // Add a message about the download
      setInput((prev) => {
        const uploadNote = `[Downloaded: ${result.name}]`;
        return prev ? `${prev}\n${uploadNote}` : uploadNote;
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
    const missionId = searchParams.get("mission");
    if (missionId) {
      const previousViewingMission = viewingMissionRef.current;
      setMissionLoading(true);
      setViewingMissionId(missionId); // Set viewing ID immediately to prevent "Agent is working..." flash
      fetchingMissionIdRef.current = missionId; // Track which mission we're loading
      loadMission(missionId)
        .then((mission) => {
          // Race condition guard: only apply if this is still the mission we want
          if (fetchingMissionIdRef.current !== missionId) return;
          setCurrentMission(mission);
          setViewingMission(mission);
          setItems(missionHistoryToItems(mission));
        })
        .catch((err) => {
          // Race condition guard: only handle error if this is still the mission we want
          if (fetchingMissionIdRef.current !== missionId) return;
          console.error("Failed to load mission:", err);
          // Show error toast for mission load failures (skip if likely a 401 during initial page load)
          const is401 = err?.message?.includes("401") || err?.status === 401;
          if (!is401) {
            toast.error("Failed to load mission");
          }

          // Revert viewing state to the previous mission to avoid filtering out events
          const fallbackMission = previousViewingMission ?? currentMissionRef.current;
          if (fallbackMission) {
            setViewingMissionId(fallbackMission.id);
            setViewingMission(fallbackMission);
            setItems(missionHistoryToItems(fallbackMission));
          } else {
            setViewingMissionId(null);
            setViewingMission(null);
            setItems([]);
          }
        })
        .finally(() => setMissionLoading(false));
    } else {
      getCurrentMission()
        .then((mission) => {
          if (mission) {
            setCurrentMission(mission);
            setViewingMission(mission);
            setItems(missionHistoryToItems(mission));
            router.replace(`/control?mission=${mission.id}`, { scroll: false });
          }
        })
        .catch((err) => {
          console.error("Failed to get current mission:", err);
        });
    }
  }, [searchParams, router, missionHistoryToItems, authRetryTrigger]);

  // Poll for running parallel missions
  useEffect(() => {
    const pollRunning = async () => {
      try {
        const running = await getRunningMissions();
        setRunningMissions(running);
        // Auto-show panel if there are parallel missions
        if (running.length > 1) {
          setShowParallelPanel(true);
        }
      } catch {
        // Ignore errors
      }
    };

    // Poll immediately and then every 3 seconds
    pollRunning();
    const interval = setInterval(pollRunning, 3000);
    return () => clearInterval(interval);
  }, []);

  // Fetch available providers and models for mission creation (retry on auth success)
  useEffect(() => {
    listProviders()
      .then((data) => {
        setProviders(data.providers);
      })
      .catch((err) => {
        console.error("Failed to fetch providers:", err);
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
          router.replace(`/control`, { scroll: false });
        }
      }
    },
    [missionItems, missionHistoryToItems, router]
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
  const handleNewMission = async (modelOverride?: string) => {
    try {
      setMissionLoading(true);
      const mission = await createMission(undefined, modelOverride);
      setCurrentMission(mission);
      setViewingMission(mission);
      setViewingMissionId(mission.id); // Also update viewing to the new mission
      setItems([]);
      setShowParallelPanel(true); // Show the missions panel so user can see the new mission
      // Refresh running missions to get accurate state
      const running = await getRunningMissions();
      setRunningMissions(running);
      router.replace(`/control?mission=${mission.id}`, { scroll: false });
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
      setShowStatusMenu(false);
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
      setShowStatusMenu(false);
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

      // If we're viewing a specific mission, filter events strictly
      if (viewingId) {
        // Event has a mission_id - must match viewing mission
        if (eventMissionId) {
          if (eventMissionId !== viewingId) {
            // Event is from a different mission - only allow status events
            if (event.type !== "status") {
              return;
            }
          }
        } else {
          // Event has NO mission_id (from main session)
          // Only show if we're viewing the current/main mission
          if (viewingId !== currentMissionId) {
            // We're viewing a parallel mission, skip main session events
            if (event.type !== "status") {
              return;
            }
          }
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
          // Status for main session - only apply if viewing main mission or no specific mission
          shouldApplyStatus = !viewingId || viewingId === currentMissionId;
        }

        if (shouldApplyStatus) {
          setQueueLen(typeof q === "number" ? q : 0);

          // Clear progress when idle
          if (newState === "idle") {
            setProgress(null);
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
        setItems((prev) => {
          // Skip if already added with this ID
          if (prev.some((item) => item.id === msgId)) return prev;

          // Check if there's a pending temp message with matching content (SSE arrived before API response)
          // We verify content to avoid mismatching with messages from other sessions/devices
          const tempIndex = prev.findIndex(
            (item) =>
              item.kind === "user" &&
              item.id.startsWith("temp-") &&
              item.content === msgContent
          );

          if (tempIndex !== -1) {
            // Replace temp ID with server ID, keeping the original content/timestamp
            const updated = [...prev];
            updated[tempIndex] = { ...updated[tempIndex], id: msgId };
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
            },
          ];
        });
        return;
      }

      if (event.type === "assistant_message" && isRecord(data)) {
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
          },
        ]);
        return;
      }

      if (event.type === "thinking" && isRecord(data)) {
        const content = String(data["content"] ?? "");
        const done = Boolean(data["done"]);

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
            updated[existingIdx] = {
              ...existing,
              // Replace content instead of appending - backend sends cumulative content
              content,
              done,
            };
            return updated;
          } else {
            return [
              ...filtered,
              {
                kind: "thinking" as const,
                id: `thinking-${Date.now()}`,
                content,
                done,
                startTime: Date.now(),
              },
            ];
          }
        });
        return;
      }

      if (event.type === "tool_call" && isRecord(data)) {
        const name = String(data["name"] ?? "");
        const isUiTool = name.startsWith("ui_");

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

        if (
          msg.includes("Stream connection failed") ||
          msg.includes("Stream ended")
        ) {
          scheduleReconnect();
        } else {
          setItems((prev) => [
            ...prev,
            { kind: "system", id: `err-${Date.now()}`, content: msg, timestamp: Date.now() },
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
      // Update connection state to show reconnecting indicator
      setConnectionState("reconnecting");
      setReconnectAttempt(reconnectAttempts);
      reconnectTimeout = setTimeout(() => {
        if (mounted) connect();
      }, delay);
    };

    const connect = () => {
      cleanup?.();
      cleanup = streamControl(handleEvent);
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

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const content = input.trim();
    if (!content) return;

    setInput("");
    setDraftInput("");

    // Generate temp ID and add message optimistically BEFORE the API call
    // This ensures messages appear in send order, not response order
    const tempId = `temp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const timestamp = Date.now();

    setItems((prev) => [
      ...prev,
      {
        kind: "user" as const,
        id: tempId,
        content,
        timestamp,
      },
    ]);

    try {
      const { id } = await postControlMessage(content);

      // Replace temp ID with server-assigned ID
      // This allows SSE handler to correctly deduplicate
      setItems((prev) =>
        prev.map((item) =>
          item.id === tempId ? { ...item, id } : item
        )
      );
    } catch (err) {
      console.error(err);
      // Remove the optimistic message on error
      setItems((prev) => prev.filter((item) => item.id !== tempId));
      toast.error("Failed to send message");
    }
  };

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
  const missionTitle = activeMission?.title
    ? activeMission.title.length > 60
      ? activeMission.title.slice(0, 60) + "..."
      : activeMission.title
    : "New Mission";

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

      {/* Header */}
      <div className="mb-6 flex flex-wrap items-center justify-between gap-4">
        <div className="flex items-center gap-4 min-w-0 flex-1">
          <div className="flex items-center gap-3 min-w-0">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-indigo-500/20">
              <Target className="h-5 w-5 text-indigo-400" />
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-2 flex-wrap">
                <h1 className="text-lg font-semibold text-white truncate">
                  {missionLoading ? "Loading..." : missionTitle}
                </h1>
                {missionStatus && (
                  <span
                    className={cn(
                      "px-2 py-0.5 rounded-full text-xs font-medium shrink-0",
                      missionStatus.className
                    )}
                  >
                    {missionStatus.label}
                  </span>
                )}
              </div>
              <p className="text-xs text-white/40 truncate">
                {activeMission
                  ? `Mission ${activeMission.id.slice(0, 8)}...`
                  : "No active mission"}
              </p>
            </div>
          </div>
        </div>

        <div className="flex items-center gap-3 shrink-0 flex-wrap">
          {activeMission && (
            <div className="relative" ref={statusMenuRef}>
              <button
                onClick={() => setShowStatusMenu(!showStatusMenu)}
                className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors"
              >
                <span className="hidden sm:inline">Set</span> Status
                <ChevronDown className="h-4 w-4" />
              </button>
              {showStatusMenu && (
                <div className="absolute right-0 top-full mt-1 w-40 rounded-lg border border-white/[0.06] bg-[#1a1a1a] py-1 shadow-xl z-10">
                  <button
                    onClick={() => handleSetStatus("completed")}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                  >
                    <CheckCircle className="h-4 w-4 text-emerald-400" />
                    Mark Complete
                  </button>
                  <button
                    onClick={() => handleSetStatus("failed")}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                  >
                    <XCircle className="h-4 w-4 text-red-400" />
                    Mark Failed
                  </button>
                  {(activeMission?.status === "interrupted" || activeMission?.status === "blocked") && (
                    <>
                      <button
                        onClick={() => handleResumeMission(false)}
                        disabled={missionLoading}
                        className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] disabled:opacity-50"
                      >
                        <PlayCircle className="h-4 w-4 text-emerald-400" />
                        {activeMission?.status === "blocked" ? "Continue Mission" : "Resume Mission"}
                      </button>
                      <button
                        onClick={() => handleResumeMission(true)}
                        disabled={missionLoading}
                        className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] disabled:opacity-50"
                        title="Delete work folder and start fresh"
                      >
                        <Trash2 className="h-4 w-4 text-orange-400" />
                        Clean & {activeMission?.status === "blocked" ? "Continue" : "Resume"}
                      </button>
                    </>
                  )}
                  {activeMission?.status !== "active" && activeMission?.status !== "interrupted" && activeMission?.status !== "blocked" && (
                    <button
                      onClick={() => handleSetStatus("active")}
                      className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                    >
                      <Clock className="h-4 w-4 text-indigo-400" />
                      Reactivate
                    </button>
                  )}
                </div>
              )}
            </div>
          )}

          <div className="relative" ref={newMissionDialogRef}>
            <button
              onClick={() => setShowNewMissionDialog(!showNewMissionDialog)}
              disabled={missionLoading}
              className="flex items-center gap-2 rounded-lg bg-indigo-500/20 px-3 py-2 text-sm font-medium text-indigo-400 hover:bg-indigo-500/30 transition-colors disabled:opacity-50"
            >
              <Plus className="h-4 w-4" />
              <span className="hidden sm:inline">New</span> Mission
            </button>
            {showNewMissionDialog && (
              <div className="absolute right-0 top-full mt-1 w-80 rounded-lg border border-white/[0.06] bg-[#1a1a1a] p-4 shadow-xl z-10">
                <h3 className="text-sm font-medium text-white mb-3">
                  Create New Mission
                </h3>
                <div className="space-y-3">
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">
                      Model
                    </label>
                    <select
                      value={newMissionModel}
                      onChange={(e) => setNewMissionModel(e.target.value)}
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none appearance-none cursor-pointer"
                      style={{
                        backgroundImage: `url("data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e")`,
                        backgroundPosition: "right 0.5rem center",
                        backgroundRepeat: "no-repeat",
                        backgroundSize: "1.5em 1.5em",
                        paddingRight: "2.5rem",
                      }}
                    >
                      <option value="" className="bg-[#1a1a1a]">
                        Auto (default)
                      </option>
                      {/* Group models by provider */}
                      {providers.map((provider) => (
                        provider.models.length > 0 && (
                          <optgroup
                            key={provider.id}
                            label={`${provider.name}${provider.billing === "subscription" ? " (included)" : ""}`}
                            className="bg-[#1a1a1a]"
                          >
                            {provider.models.map((model) => (
                              <option key={model.id} value={model.id} className="bg-[#1a1a1a]">
                                {model.name}
                              </option>
                            ))}
                          </optgroup>
                        )
                      ))}
                    </select>
                    <p className="text-xs text-white/30 mt-1.5">
                      Auto uses Claude Opus 4.5
                    </p>
                  </div>
                  <div className="flex gap-2 pt-1">
                    <button
                      onClick={() => {
                        setShowNewMissionDialog(false);
                        setNewMissionModel("");
                      }}
                      className="flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors"
                    >
                      Cancel
                    </button>
                    <button
                      onClick={() => {
                        handleNewMission(newMissionModel || undefined);
                        setShowNewMissionDialog(false);
                        setNewMissionModel("");
                      }}
                      disabled={missionLoading}
                      className="flex-1 rounded-lg bg-indigo-500 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                    >
                      Create
                    </button>
                  </div>
                </div>
              </div>
            )}
          </div>

          {/* Parallel missions indicator */}
          {(runningMissions.length > 0 || currentMission) && (
            <button
              onClick={() => setShowParallelPanel(!showParallelPanel)}
              className={cn(
                "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
                showParallelPanel
                  ? "border-indigo-500/30 bg-indigo-500/10 text-indigo-400"
                  : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]"
              )}
            >
              <Layers className="h-4 w-4" />
              <span className="font-medium tabular-nums">
                {runningMissions.length}
              </span>
              <span className="hidden sm:inline">Running</span>
            </button>
          )}

          {/* Desktop stream toggle with display selector */}
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
                  "flex items-center gap-1 rounded-r-lg border-y border-r px-2 py-2 text-sm transition-colors",
                  showDesktopStream
                    ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-400"
                    : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]"
                )}
                title="Select display"
              >
                <span className="text-xs font-mono">{desktopDisplayId}</span>
                <ChevronDown className="h-3 w-3" />
              </button>
              {showDisplaySelector && (
                <div className="absolute right-0 top-full mt-1 z-50 min-w-[120px] rounded-lg border border-white/[0.06] bg-[#121214] shadow-xl">
                  {[":99", ":100", ":101", ":102"].map((display) => (
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
                  ))}
                </div>
              )}
            </div>
          </div>

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

      {/* Running Missions Panel - Compact horizontal layout */}
      {showParallelPanel && (runningMissions.length > 0 || currentMission) && (
        <div className="mb-4 flex items-center gap-2 overflow-x-auto pb-1">
          <div className="flex items-center gap-1.5 shrink-0 text-white/40">
            <Layers className="h-3.5 w-3.5" />
            <span className="text-xs font-medium">Running Missions</span>
            <button
              onClick={async () => {
                const running = await getRunningMissions();
                setRunningMissions(running);
              }}
              className="p-0.5 rounded hover:bg-white/[0.04] hover:text-white/70 transition-colors"
              title="Refresh"
            >
              <RefreshCw className="h-3 w-3" />
            </button>
          </div>

          {/* Show current mission first if it's not in running missions */}
          {currentMission &&
            !runningMissions.some(
              (m) => m.mission_id === currentMission.id
            ) && (
              <div
                onClick={() => handleViewMission(currentMission.id)}
                className={cn(
                  "flex items-center gap-2 rounded-lg border px-2.5 py-1.5 transition-colors cursor-pointer shrink-0",
                  viewingMissionId === currentMission.id
                    ? "border-indigo-500/30 bg-indigo-500/10"
                    : "border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04]"
                )}
              >
                <div className="h-1.5 w-1.5 rounded-full shrink-0 bg-emerald-400" />
                <span className="text-xs font-medium text-white truncate max-w-[140px]">
                  {currentMission.model_override?.split("/").pop() || "Default"}
                </span>
                <span className="text-[10px] text-white/40 tabular-nums">
                  {currentMission.id.slice(0, 8)}
                </span>
                {viewingMissionId === currentMission.id && (
                  <Check className="h-3 w-3 text-indigo-400" />
                )}
              </div>
            )}

          {runningMissions.map((mission) => {
            const isViewingMission = viewingMissionId === mission.mission_id;
            const isStalled =
              mission.state === "running" &&
              mission.seconds_since_activity > 60;
            const isSeverlyStalled =
              mission.state === "running" &&
              mission.seconds_since_activity > 120;

            return (
              <div
                key={mission.mission_id}
                onClick={() => handleViewMission(mission.mission_id)}
                className={cn(
                  "flex items-center gap-2 rounded-lg border px-2.5 py-1.5 transition-colors cursor-pointer shrink-0",
                  isViewingMission
                    ? "border-indigo-500/30 bg-indigo-500/10"
                    : isSeverlyStalled
                    ? "border-red-500/30 bg-red-500/10"
                    : isStalled
                    ? "border-amber-500/30 bg-amber-500/10"
                    : "border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04]"
                )}
              >
                <div
                  className={cn(
                    "h-1.5 w-1.5 rounded-full shrink-0",
                    isSeverlyStalled
                      ? "bg-red-400"
                      : isStalled
                      ? "bg-amber-400 animate-pulse"
                      : mission.state === "running"
                      ? "bg-emerald-400 animate-pulse"
                      : "bg-amber-400"
                  )}
                />
                <span className="text-xs font-medium text-white truncate max-w-[140px]">
                  {mission.model_override?.split("/").pop() || "Default"}
                </span>
                <span className="text-[10px] text-white/40 tabular-nums">
                  {mission.mission_id.slice(0, 8)}
                </span>
                {isStalled && (
                  <span className="text-[10px] text-amber-400 tabular-nums">
                    ⚠️ {Math.floor(mission.seconds_since_activity)}s
                  </span>
                )}
                {isViewingMission && (
                  <Check className="h-3 w-3 text-indigo-400" />
                )}
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleCancelMission(mission.mission_id);
                  }}
                  className="p-0.5 rounded hover:bg-white/[0.08] text-white/30 hover:text-red-400 transition-colors"
                  title="Cancel mission"
                >
                  <XCircle className="h-3 w-3" />
                </button>
              </div>
            );
          })}
        </div>
      )}

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

                    {/* Quick Action Templates */}
                    <div className="mt-6 grid grid-cols-2 gap-2 max-w-md mx-auto">
                      <button
                        onClick={() => setInput("Read the files in /root/context and summarize what they contain")}
                        className="flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/60 hover:bg-white/[0.08] hover:text-white/80 transition-colors text-left"
                      >
                        <FileText className="h-4 w-4 text-indigo-400 shrink-0" />
                        <span>Analyze context files</span>
                      </button>
                      <button
                        onClick={() => setInput("Search the web for the latest news about ")}
                        className="flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/60 hover:bg-white/[0.08] hover:text-white/80 transition-colors text-left"
                      >
                        <Globe className="h-4 w-4 text-emerald-400 shrink-0" />
                        <span>Search the web</span>
                      </button>
                      <button
                        onClick={() => setInput("Write a Python script that ")}
                        className="flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/60 hover:bg-white/[0.08] hover:text-white/80 transition-colors text-left"
                      >
                        <Code className="h-4 w-4 text-amber-400 shrink-0" />
                        <span>Write code</span>
                      </button>
                      <button
                        onClick={() => setInput("Run the command: ")}
                        className="flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/60 hover:bg-white/[0.08] hover:text-white/80 transition-colors text-left"
                      >
                        <Terminal className="h-4 w-4 text-cyan-400 shrink-0" />
                        <span>Run command</span>
                      </button>
                    </div>

                    <p className="mt-4 text-xs text-white/30">
                      Tip: Paste files directly to upload to context folder
                    </p>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="mx-auto max-w-3xl space-y-6">
              {items.map((item) => {
                if (item.kind === "user") {
                  return (
                    <div key={item.id} className="flex justify-end gap-3 group">
                      <CopyButton
                        text={item.content}
                        className="self-start mt-2"
                      />
                      <div className="max-w-[80%]">
                        <div className="rounded-2xl rounded-br-md bg-indigo-500 px-4 py-3 text-white selection-light">
                          <p className="whitespace-pre-wrap text-sm">
                            {item.content}
                          </p>
                        </div>
                        <div className="mt-1 text-right">
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
                      <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
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
                        <MarkdownContent content={item.content} />
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

                if (item.kind === "thinking") {
                  return <ThinkingItem key={item.id} item={item} />;
                }

                if (item.kind === "tool") {
                  // UI tools get special interactive rendering
                  if (item.isUiTool) {
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
                          <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
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
                          <div className="max-w-[90%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
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

                  // Non-UI tools use the collapsible ToolCallItem component
                  return <ToolCallItem key={item.id} item={item} />;
                }

                // system
                return (
                  <div key={item.id} className="flex justify-start gap-3">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.04]">
                      <Ban className="h-4 w-4 text-white/40" />
                    </div>
                    <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.02] border border-white/[0.04] px-4 py-3">
                      <p className="whitespace-pre-wrap text-sm text-white/60">
                        {item.content}
                      </p>
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
                ) && (
                  <div className="flex justify-start gap-3 animate-fade-in">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                      <Bot className="h-4 w-4 text-indigo-400 animate-pulse" />
                    </div>
                    <div className="rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                      <div className="flex items-center gap-2">
                        <Loader className="h-4 w-4 text-indigo-400 animate-spin" />
                        <span className="text-sm text-white/60">
                          Agent is working...
                        </span>
                      </div>
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

          <form
            onSubmit={handleSubmit}
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

            <textarea
              ref={textareaRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (input.trim()) {
                    handleSubmit(e);
                  }
                }
              }}
              placeholder="Message the root agent… (paste files to upload)"
              rows={1}
              className="flex-1 rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-[border-color,height] duration-150 ease-out resize-none overflow-y-auto leading-5"
              style={{ minHeight: "46px" }}
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
        </div>
      </div>

        {/* Desktop Stream Panel */}
        {showDesktopStream && (
          <div className="flex-1 min-h-0 transition-all duration-300 animate-fade-in">
            <DesktopStream
              displayId={desktopDisplayId}
              className="h-full"
              onClose={() => setShowDesktopStream(false)}
            />
          </div>
        )}
      </div>
    </div>
  );
}
