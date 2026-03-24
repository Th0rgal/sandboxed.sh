"use client";

import { useEffect, useState, useRef } from "react";
import useSWR from "swr";
import { cn } from "@/lib/utils";
import { listTasks, stopTask, type Task, type TaskStep } from "@/lib/api/tasks";
import { RelativeTime } from "@/components/ui/relative-time";
import {
  Clock,
  Loader,
  CheckCircle,
  XCircle,
  CircleDot,
  ChevronDown,
  ChevronRight,
  Square,
  Terminal,
  Bot,
  ListTodo,
} from "lucide-react";

// ---------------------------------------------------------------------------
// Status helpers
// ---------------------------------------------------------------------------

const statusConfig: Record<string, { color: string; bg: string; label: string }> = {
  pending:   { color: "text-amber-400",   bg: "bg-amber-500/10",   label: "Pending" },
  running:   { color: "text-indigo-400",  bg: "bg-indigo-500/10",  label: "Running" },
  completed: { color: "text-emerald-400", bg: "bg-emerald-500/10", label: "Done" },
  failed:    { color: "text-red-400",     bg: "bg-red-500/10",     label: "Failed" },
  cancelled: { color: "text-white/40",    bg: "bg-white/[0.04]",   label: "Cancelled" },
};

function StatusBadge({ status }: { status: string }) {
  const cfg = statusConfig[status] ?? statusConfig.pending;
  const Icon =
    status === "running"   ? Loader :
    status === "completed" ? CheckCircle :
    status === "failed"    ? XCircle :
    status === "cancelled" ? XCircle :
    CircleDot;
  return (
    <span className={cn("inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium", cfg.color, cfg.bg)}>
      <Icon className={cn("w-3 h-3", status === "running" && "animate-spin")} />
      {cfg.label}
    </span>
  );
}

function ModeBadge({ mode }: { mode: string }) {
  const isCommand = mode === "command";
  return (
    <span className={cn(
      "inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-mono font-medium",
      isCommand ? "text-cyan-400 bg-cyan-500/10" : "text-violet-400 bg-violet-500/10"
    )}>
      {isCommand ? <Terminal className="w-2.5 h-2.5" /> : <Bot className="w-2.5 h-2.5" />}
      {isCommand ? "cmd" : "agent"}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Step timeline
// ---------------------------------------------------------------------------

const stepStatusColor: Record<string, string> = {
  started:   "text-indigo-400",
  completed: "text-emerald-400",
  failed:    "text-red-400",
  unknown:   "text-white/40",
};

function StepTimeline({ steps }: { steps: TaskStep[] }) {
  if (steps.length === 0) return null;
  return (
    <div className="mt-3 space-y-1">
      <p className="text-[10px] uppercase tracking-widest text-white/30 mb-2">Steps</p>
      {steps.map((step, i) => (
        <div key={i} className="flex items-start gap-2 text-xs">
          <div className={cn("mt-0.5 w-1.5 h-1.5 rounded-full flex-shrink-0",
            step.status === "completed" ? "bg-emerald-400" :
            step.status === "failed"    ? "bg-red-400" :
            step.status === "started"   ? "bg-indigo-400 animate-pulse" : "bg-white/20"
          )} />
          <div className="flex-1 min-w-0">
            <span className="text-white/80 font-medium">{step.name}</span>
            {step.iteration != null && (
              <span className="ml-1 text-white/30">#{step.iteration}</span>
            )}
            <span className={cn("ml-2", stepStatusColor[step.status] ?? stepStatusColor.unknown)}>
              {step.status}
            </span>
            {step.duration_s != null && (
              <span className="ml-2 text-white/30">{step.duration_s.toFixed(1)}s</span>
            )}
            {step.metadata && (
              <span className="ml-2 text-white/40 font-mono text-[10px]">
                {Object.entries(step.metadata)
                  .map(([k, v]) => `${k}=${typeof v === "number" ? v.toFixed(3) : String(v)}`)
                  .join(" · ")}
              </span>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Log viewer
// ---------------------------------------------------------------------------

function LogViewer({ task }: { task: Task }) {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [task.log.length]);

  if (task.log.length === 0) {
    return <p className="text-white/30 text-xs italic py-2">No log entries yet.</p>;
  }

  return (
    <div className="font-mono text-xs max-h-64 overflow-y-auto space-y-0.5 pr-1">
      {task.log.map((entry, i) => (
        <div key={i} className={cn(
          "flex gap-2",
          entry.entry_type === "error" ? "text-red-400/80" : "text-white/60"
        )}>
          <span className="text-white/20 flex-shrink-0 select-none">
            {new Date(entry.timestamp).toLocaleTimeString("en-GB", { hour12: false })}
          </span>
          <span className="break-all whitespace-pre-wrap">{entry.content}</span>
        </div>
      ))}
      <div ref={bottomRef} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Task row
// ---------------------------------------------------------------------------

function TaskRow({ task, onStop, stopping }: { task: Task; onStop: (id: string) => void; stopping: boolean }) {
  const [expanded, setExpanded] = useState(false);

  const started = task.started_at ?? task.log[0]?.timestamp;
  const elapsed = started
    ? Math.round((Date.now() - new Date(started).getTime()) / 1000)
    : null;

  return (
    <div className="border border-white/[0.06] rounded-lg bg-white/[0.02] overflow-hidden">
      {/* Header row */}
      <button
        className="w-full flex items-center gap-3 px-4 py-3 text-left hover:bg-white/[0.03] transition-colors"
        onClick={() => setExpanded((v) => !v)}
      >
        {expanded ? (
          <ChevronDown className="w-3.5 h-3.5 text-white/30 flex-shrink-0" />
        ) : (
          <ChevronRight className="w-3.5 h-3.5 text-white/30 flex-shrink-0" />
        )}

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-white/90 text-sm font-medium truncate">{task.task}</span>
            <ModeBadge mode={task.mode} />
            {task.workspace_name && (
              <span className="text-white/30 text-[10px] font-mono">{task.workspace_name}</span>
            )}
          </div>
          {task.result && (
            <p className={cn("text-xs mt-0.5 truncate",
              task.status === "failed" ? "text-red-400/70" : "text-white/40"
            )}>
              {task.result}
            </p>
          )}
        </div>

        <div className="flex items-center gap-3 flex-shrink-0">
          {elapsed != null && task.status === "running" && (
            <span className="text-white/30 text-xs flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {elapsed}s
            </span>
          )}
          {task.status !== "running" && task.status !== "pending" && (task.completed_at ?? task.started_at) && (
            <RelativeTime date={(task.completed_at ?? task.started_at)!} className="text-white/30 text-xs" />
          )}
          <StatusBadge status={task.status} />
          {task.status === "running" && (
            <button
              onClick={(e) => { e.stopPropagation(); onStop(task.id); }}
              disabled={stopping}
              className="p-1 rounded hover:bg-red-500/10 text-white/30 hover:text-red-400 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              title={stopping ? "Stopping…" : "Stop task"}
            >
              {stopping
                ? <Loader className="w-3.5 h-3.5 animate-spin" />
                : <Square className="w-3.5 h-3.5" />}
            </button>
          )}
        </div>
      </button>

      {/* Expanded detail */}
      {expanded && (
        <div className="px-4 pb-4 border-t border-white/[0.04]">
          <div className="mt-3 grid grid-cols-2 gap-x-6 gap-y-1 text-xs text-white/40 mb-3">
            <span>ID: <span className="font-mono text-white/30">{task.id.slice(0, 8)}</span></span>
            {task.mode === "agent" && task.model && (
              <span>Model: <span className="text-white/30">{task.model}</span></span>
            )}
            {task.iterations > 0 && (
              <span>Iterations: <span className="text-white/30">{task.iterations}</span></span>
            )}
            {(task.steps?.length ?? 0) > 0 && (
              <span>Steps: <span className="text-white/30">{task.steps!.length}</span></span>
            )}
            {task.started_at && (
              <span>Started: <RelativeTime date={task.started_at} className="text-white/30" /></span>
            )}
            {task.completed_at && (
              <span>Completed: <RelativeTime date={task.completed_at} className="text-white/30" /></span>
            )}
            {task.duration_secs != null && (
              <span>Duration: <span className="text-white/30">{task.duration_secs.toFixed(1)}s</span></span>
            )}
          </div>
          <StepTimeline steps={task.steps ?? []} />
          <div className="mt-3">
            <p className="text-[10px] uppercase tracking-widest text-white/30 mb-2">Log</p>
            <LogViewer task={task} />
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function TasksPage() {
  const [stoppingIds, setStoppingIds] = useState<Set<string>>(new Set());

  const { data: tasks = [], mutate } = useSWR<Task[]>(
    "tasks",
    listTasks,
    { refreshInterval: 2000 }
  );

  const handleStop = async (id: string) => {
    setStoppingIds((s) => new Set(s).add(id));
    try {
      await stopTask(id);
      await mutate();
    } finally {
      setStoppingIds((s) => { const n = new Set(s); n.delete(id); return n; });
    }
  };

  const running = tasks.filter((t) => t.status === "running" || t.status === "pending");
  const done    = tasks
    .filter((t) => t.status !== "running" && t.status !== "pending")
    .sort((a, b) => {
      const ta = new Date(a.completed_at ?? a.started_at ?? a.created_at ?? 0).getTime();
      const tb = new Date(b.completed_at ?? b.started_at ?? b.created_at ?? 0).getTime();
      return tb - ta;
    });

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex-shrink-0 px-6 py-5 border-b border-white/[0.06]">
        <div className="flex items-center gap-3">
          <ListTodo className="w-5 h-5 text-white/40" />
          <div>
            <h1 className="text-lg font-semibold text-white/90">Tasks</h1>
            <p className="text-xs text-white/40">
              Background shell commands and agent tasks.{" "}
              {running.length > 0 && (
                <span className="text-indigo-400">{running.length} running</span>
              )}
            </p>
          </div>
        </div>
      </div>

      {/* Body */}
      <div className="flex-1 overflow-y-auto px-6 py-4 space-y-6">
        {tasks.length === 0 && (
          <div className="text-center py-16 text-white/30">
            <ListTodo className="w-8 h-8 mx-auto mb-3 opacity-40" />
            <p className="text-sm">No tasks yet.</p>
            <p className="text-xs mt-1 opacity-60">
              POST /api/task with a <code className="font-mono">command</code> field to run a script in a workspace.
            </p>
          </div>
        )}

        {running.length > 0 && (
          <section>
            <p className="text-[10px] uppercase tracking-widest text-white/30 mb-2">Active</p>
            <div className="space-y-2">
              {running.map((t) => (
                <TaskRow key={t.id} task={t} onStop={handleStop} stopping={stoppingIds.has(t.id)} />
              ))}
            </div>
          </section>
        )}

        {done.length > 0 && (
          <section>
            <p className="text-[10px] uppercase tracking-widest text-white/30 mb-2">History</p>
            <div className="space-y-2">
              {done.map((t) => (
                <TaskRow key={t.id} task={t} onStop={handleStop} stopping={stoppingIds.has(t.id)} />
              ))}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
