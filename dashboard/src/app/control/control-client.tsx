"use client";

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import Markdown from "react-markdown";
import { toast } from "sonner";
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
  getCurrentMission,
  uploadFile,
  getProgress,
  getRunningMissions,
  cancelMission,
  type ControlRunState,
  type Mission,
  type MissionStatus,
  type RunningMissionInfo,
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

type ChatItem =
  | {
      kind: "user";
      id: string;
      content: string;
    }
  | {
      kind: "assistant";
      id: string;
      content: string;
      success: boolean;
      costCents: number;
      model: string | null;
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
    }
  | {
      kind: "system";
      id: string;
      content: string;
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
  useEffect(() => {
    if (item.done && expanded && !hasAutoCollapsedRef.current) {
      const timer = setTimeout(() => {
        setExpanded(false);
        hasAutoCollapsedRef.current = true;
      }, 500);
      return () => clearTimeout(timer);
    }
  }, [item.done, expanded]);

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
          expanded ? "max-h-80 opacity-100 mt-2" : "max-h-0 opacity-0"
        )}
      >
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
          <div className="text-xs text-white/50 whitespace-pre-wrap overflow-y-auto max-h-64 leading-relaxed">
            {item.content}
          </div>
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

  // Progress state (for "Subtask X of Y" indicator)
  const [progress, setProgress] = useState<{
    total: number;
    completed: number;
    current: string | null;
    depth: number;
  } | null>(null);

  // Mission state
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [showStatusMenu, setShowStatusMenu] = useState(false);
  const [missionLoading, setMissionLoading] = useState(false);

  // Parallel missions state
  const [runningMissions, setRunningMissions] = useState<RunningMissionInfo[]>([]);
  const [showParallelPanel, setShowParallelPanel] = useState(false);
  
  // Track which mission's events we're viewing (for parallel missions)
  // This can differ from currentMission when viewing a parallel mission
  const [viewingMissionId, setViewingMissionId] = useState<string | null>(null);
  
  // Store items per mission to preserve context when switching
  const [missionItems, setMissionItems] = useState<Record<string, ChatItem[]>>({});

  // Attachment state
  const [attachments, setAttachments] = useState<
    { file: File; uploading: boolean }[]
  >([]);
  const [uploadQueue, setUploadQueue] = useState<string[]>([]);

  const isBusy = runState !== "idle";

  const streamCleanupRef = useRef<null | (() => void)>(null);
  const statusMenuRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const viewingMissionIdRef = useRef<string | null>(null);
  
  // Keep ref in sync with state
  useEffect(() => {
    viewingMissionIdRef.current = viewingMissionId;
  }, [viewingMissionId]);

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
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

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
  }, []);

  // Handle file upload
  const handleFileUpload = async (file: File) => {
    setUploadQueue((prev) => [...prev, file.name]);

    try {
      const result = await uploadFile(file, "/root/context/");
      toast.success(`Uploaded ${result.name} to /root/context/`);

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
    }
  };

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
    return mission.history.map((entry, i) => {
      if (entry.role === "user") {
        return {
          kind: "user" as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
        };
      } else {
        return {
          kind: "assistant" as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
          success: true,
          costCents: 0,
          model: null,
        };
      }
    });
  }, []);

  // Load mission from URL param on mount
  useEffect(() => {
    const missionId = searchParams.get("mission");
    if (missionId) {
      setMissionLoading(true);
      loadMission(missionId)
        .then((mission) => {
          setCurrentMission(mission);
          setItems(missionHistoryToItems(mission));
        })
        .catch((err) => {
          console.error("Failed to load mission:", err);
          toast.error("Failed to load mission");
        })
        .finally(() => setMissionLoading(false));
    } else {
      getCurrentMission()
        .then((mission) => {
          if (mission) {
            setCurrentMission(mission);
            setItems(missionHistoryToItems(mission));
            router.replace(`/control?mission=${mission.id}`, { scroll: false });
          }
        })
        .catch((err) => {
          console.error("Failed to get current mission:", err);
        });
    }
  }, [searchParams, router, missionHistoryToItems]);

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

  // Handle switching which mission we're viewing
  const handleViewMission = useCallback(async (missionId: string) => {
    setViewingMissionId(missionId);
    
    // Check if we already have items for this mission
    if (missionItems[missionId]) {
      setItems(missionItems[missionId]);
      return;
    }
    
    // Load mission history from API
    try {
      const mission = await loadMission(missionId);
      const historyItems = missionHistoryToItems(mission);
      setMissionItems(prev => ({ ...prev, [missionId]: historyItems }));
      setItems(historyItems);
    } catch (err) {
      console.error("Failed to load mission:", err);
      // Still set viewing ID even if load fails
    }
  }, [missionItems, missionHistoryToItems]);

  // Sync viewingMissionId with currentMission
  useEffect(() => {
    if (currentMission && !viewingMissionId) {
      setViewingMissionId(currentMission.id);
    }
  }, [currentMission, viewingMissionId]);

  // Save current items to missionItems when they change
  useEffect(() => {
    if (viewingMissionId && items.length > 0) {
      setMissionItems(prev => ({ ...prev, [viewingMissionId]: items }));
    }
  }, [items, viewingMissionId]);

  // Handle creating a new mission
  const handleNewMission = async () => {
    try {
      setMissionLoading(true);
      const mission = await createMission();
      setCurrentMission(mission);
      setItems([]);
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
    if (!currentMission) return;
    try {
      await setMissionStatus(currentMission.id, status);
      setCurrentMission({ ...currentMission, status });
      setShowStatusMenu(false);
      toast.success(`Mission marked as ${status}`);
    } catch (err) {
      console.error("Failed to set mission status:", err);
      toast.error("Failed to update mission status");
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
      if (isRecord(data) && data["mission_id"]) {
        const eventMissionId = String(data["mission_id"]);
        const viewingId = viewingMissionIdRef.current;
        
        // If we're viewing a specific mission and this event is for a different one,
        // store it for that mission but don't display it
        if (viewingId && eventMissionId !== viewingId) {
          // Store event for the other mission (so it's there when we switch)
          // For now, we'll just ignore non-status events for other missions
          // Status events should still update the running missions indicator
          if (event.type !== "status") {
            return;
          }
        }
      }

      if (event.type === "status" && isRecord(data)) {
        reconnectAttempts = 0;
        const st = data["state"];
        const newState =
          typeof st === "string" ? (st as ControlRunState) : "idle";
        const q = data["queue_len"];
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
        return;
      }

      if (event.type === "user_message" && isRecord(data)) {
        setItems((prev) => [
          ...prev,
          {
            kind: "user",
            id: String(data["id"] ?? Date.now()),
            content: String(data["content"] ?? ""),
          },
        ]);
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
              content: existing.content + "\n\n---\n\n" + content,
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
        if (!name.startsWith("ui_")) return;

        setItems((prev) => [
          ...prev,
          {
            kind: "tool",
            id: `tool-${String(data["tool_call_id"] ?? Date.now())}`,
            toolCallId: String(data["tool_call_id"] ?? ""),
            name,
            args: data["args"],
          },
        ]);
        return;
      }

      if (event.type === "tool_result" && isRecord(data)) {
        const name = String(data["name"] ?? "");
        if (!name.startsWith("ui_")) return;

        const toolCallId = String(data["tool_call_id"] ?? "");
        setItems((prev) =>
          prev.map((it) =>
            it.kind === "tool" && it.toolCallId === toolCallId
              ? { ...it, result: data["result"] }
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
            { kind: "system", id: `err-${Date.now()}`, content: msg },
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

    try {
      await postControlMessage(content);
    } catch (err) {
      console.error(err);
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

  const missionStatus = currentMission
    ? missionStatusLabel(currentMission.status)
    : null;
  const missionTitle = currentMission?.title
    ? currentMission.title.length > 60
      ? currentMission.title.slice(0, 60) + "..."
      : currentMission.title
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
                {currentMission
                  ? `Mission ${currentMission.id.slice(0, 8)}...`
                  : "No active mission"}
              </p>
            </div>
          </div>
        </div>

        <div className="flex items-center gap-3 shrink-0 flex-wrap">
          {currentMission && (
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
                  {currentMission.status !== "active" && (
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

          <button
            onClick={handleNewMission}
            disabled={missionLoading}
            className="flex items-center gap-2 rounded-lg bg-indigo-500/20 px-3 py-2 text-sm font-medium text-indigo-400 hover:bg-indigo-500/30 transition-colors disabled:opacity-50"
          >
            <Plus className="h-4 w-4" />
            <span className="hidden sm:inline">New</span> Mission
          </button>

          {/* Parallel missions indicator */}
          {runningMissions.length > 0 && (
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
              <span className="font-medium tabular-nums">{runningMissions.length}</span>
              <span className="hidden sm:inline">Running</span>
            </button>
          )}

          {/* Status panel */}
          <div className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2">
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
            <div className="flex items-center gap-1.5">
              <span className="text-[10px] uppercase tracking-wider text-white/40">
                Queue
              </span>
              <span className="text-sm font-medium text-white/70 tabular-nums">
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

      {/* Parallel Missions Panel */}
      {showParallelPanel && runningMissions.length > 0 && (
        <div className="mb-4 rounded-xl border border-white/[0.06] bg-white/[0.02] p-4">
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-2">
              <Layers className="h-4 w-4 text-indigo-400" />
              <h3 className="text-sm font-medium text-white">Running Missions</h3>
            </div>
            <button
              onClick={async () => {
                const running = await getRunningMissions();
                setRunningMissions(running);
              }}
              className="p-1 rounded hover:bg-white/[0.04] text-white/40 hover:text-white/70 transition-colors"
              title="Refresh"
            >
              <RefreshCw className="h-3.5 w-3.5" />
            </button>
          </div>
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
            {runningMissions.map((mission) => {
              const isViewingMission = viewingMissionId === mission.mission_id;
              return (
                <div
                  key={mission.mission_id}
                  onClick={() => handleViewMission(mission.mission_id)}
                  className={cn(
                    "flex items-center justify-between rounded-lg border p-3 transition-colors cursor-pointer",
                    isViewingMission
                      ? "border-indigo-500/30 bg-indigo-500/10"
                      : "border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.04]"
                  )}
                >
                  <div className="flex items-center gap-3 min-w-0">
                    <div
                      className={cn(
                        "h-2 w-2 rounded-full shrink-0",
                        mission.state === "running" ? "bg-emerald-400 animate-pulse" : "bg-amber-400"
                      )}
                    />
                    <div className="min-w-0">
                      <p className="text-sm font-medium text-white truncate">
                        {mission.model_override || "Default Model"}
                      </p>
                      <p className="text-xs text-white/40 truncate">
                        {mission.mission_id.slice(0, 8)}... • {mission.state}
                      </p>
                    </div>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    {isViewingMission ? (
                      <Check className="h-4 w-4 text-indigo-400" />
                    ) : (
                      <span className="text-xs text-white/40">Click to view</span>
                    )}
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleCancelMission(mission.mission_id);
                      }}
                      className="p-1 rounded hover:bg-white/[0.04] text-white/40 hover:text-red-400 transition-colors"
                      title="Cancel mission"
                    >
                      <XCircle className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Chat container */}
      <div className="flex-1 min-h-0 flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden relative">
        {/* Messages */}
        <div ref={containerRef} className="flex-1 overflow-y-auto p-6">
          {items.length === 0 ? (
            <div className="flex h-full items-center justify-center">
              <div className="text-center">
                <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10">
                  {runState === "running" ? (
                    <Loader className="h-8 w-8 text-indigo-400 animate-spin" />
                  ) : (
                    <Bot className="h-8 w-8 text-indigo-400" />
                  )}
                </div>
                {missionLoading ? (
                  <Shimmer className="max-w-xs mx-auto" />
                ) : runState === "running" ? (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      Agent is working...
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      Processing your request. Updates will appear here as they
                      arrive.
                    </p>
                  </>
                ) : currentMission && currentMission.status !== "active" ? (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      No conversation history
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      This mission was {currentMission.status} without any
                      messages.
                      {currentMission.status === "completed" &&
                        " You can reactivate it to continue."}
                    </p>
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
                    <p className="mt-1 text-xs text-white/30">
                      Tip: Paste files directly to upload to /root/context/
                    </p>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="mx-auto max-w-3xl space-y-6">
              {/* Show streaming indicator when running but no active thinking/phase */}
              {runState === "running" &&
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

              {items.map((item) => {
                if (item.kind === "user") {
                  return (
                    <div key={item.id} className="flex justify-end gap-3 group">
                      <CopyButton
                        text={item.content}
                        className="self-start mt-2"
                      />
                      <div className="max-w-[80%] rounded-2xl rounded-br-md bg-indigo-500 px-4 py-3 text-white selection-light">
                        <p className="whitespace-pre-wrap text-sm">
                          {item.content}
                        </p>
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
                        </div>
                        <div className="prose-glass text-sm [&_p]:my-2 [&_code]:text-xs">
                          <Markdown>{item.content}</Markdown>
                        </div>
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

                  return (
                    <div key={item.id} className="flex justify-start gap-3">
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                        <Bot className="h-4 w-4 text-indigo-400" />
                      </div>
                      <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                        <p className="text-sm text-white/60">
                          Unsupported Tool:{" "}
                          <span className="font-mono text-indigo-400">
                            {item.name}
                          </span>
                        </p>
                      </div>
                    </div>
                  );
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
          {/* Upload queue */}
          {uploadQueue.length > 0 && (
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

          <form
            onSubmit={handleSubmit}
            className="mx-auto flex max-w-3xl gap-3 items-end"
          >
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              className="p-3 rounded-xl border border-white/[0.06] bg-white/[0.02] text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors shrink-0"
              title="Attach files"
            >
              <Paperclip className="h-5 w-5" />
            </button>

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
    </div>
  );
}
