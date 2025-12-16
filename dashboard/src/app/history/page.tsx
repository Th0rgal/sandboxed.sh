"use client";

import { useEffect, useState, useRef } from "react";
import Link from "next/link";
import { cn } from "@/lib/utils";
import { listTasks, listRuns, TaskState, Run } from "@/lib/api";
import {
  CheckCircle,
  XCircle,
  Clock,
  Loader,
  Ban,
  ArrowRight,
  Search,
  MessageSquare,
} from "lucide-react";

const statusIcons = {
  pending: Clock,
  running: Loader,
  completed: CheckCircle,
  failed: XCircle,
  cancelled: Ban,
};

const statusConfig = {
  pending: { color: "text-amber-400", bg: "bg-amber-500/10" },
  running: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  completed: { color: "text-emerald-400", bg: "bg-emerald-500/10" },
  failed: { color: "text-red-400", bg: "bg-red-500/10" },
  cancelled: { color: "text-white/40", bg: "bg-white/[0.04]" },
};

export default function HistoryPage() {
  const [tasks, setTasks] = useState<TaskState[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState<string>("all");
  const [search, setSearch] = useState("");
  const fetchedRef = useRef(false);

  useEffect(() => {
    if (fetchedRef.current) return;
    fetchedRef.current = true;

    const fetchData = async () => {
      try {
        const [tasksData, runsData] = await Promise.all([
          listTasks().catch(() => []),
          listRuns().catch(() => ({ runs: [] })),
        ]);
        setTasks(tasksData);
        setRuns(runsData.runs || []);
      } catch (error) {
        console.error("Failed to fetch data:", error);
      } finally {
        setLoading(false);
      }
    };

    fetchData();
  }, []);

  const filteredTasks = tasks.filter((task) => {
    if (filter !== "all" && task.status !== filter) return false;
    if (search && !task.task.toLowerCase().includes(search.toLowerCase()))
      return false;
    return true;
  });

  const filteredRuns = runs.filter((run) => {
    if (filter !== "all" && run.status !== filter) return false;
    if (search && !run.input_text.toLowerCase().includes(search.toLowerCase()))
      return false;
    return true;
  });

  const hasData = filteredTasks.length > 0 || filteredRuns.length > 0;

  return (
    <div className="p-6">
      {/* Header */}
      <div className="mb-6">
        <h1 className="text-xl font-semibold text-white">History</h1>
        <p className="mt-1 text-sm text-white/50">
          View all past and current tasks
        </p>
      </div>

      {/* Filters */}
      <div className="mb-6 flex items-center gap-4">
        <div className="relative flex-1 max-w-md">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
          <input
            type="text"
            placeholder="Search tasks..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] py-2.5 pl-10 pr-4 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
          />
        </div>

        <div className="inline-flex rounded-lg bg-white/[0.02] border border-white/[0.04] p-1">
          {["all", "running", "completed", "failed"].map((status) => (
            <button
              key={status}
              onClick={() => setFilter(status)}
              className={cn(
                "px-3 py-1.5 rounded-md text-xs font-medium transition-colors capitalize",
                filter === status
                  ? "bg-white/[0.08] text-white"
                  : "text-white/40 hover:text-white/60"
              )}
            >
              {status}
            </button>
          ))}
        </div>
      </div>

      {/* Content */}
      {loading ? (
        <div className="py-12 text-center">
          <Loader className="h-8 w-8 animate-spin text-indigo-400 mx-auto" />
        </div>
      ) : !hasData ? (
        <div className="flex flex-col items-center py-16 text-center">
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
            <MessageSquare className="h-8 w-8 text-white/30" />
          </div>
          <p className="text-white/80">No history yet</p>
          <p className="mt-2 text-sm text-white/40">
            Start a conversation in the{" "}
            <Link
              href="/control"
              className="text-indigo-400 hover:text-indigo-300"
            >
              Control
            </Link>{" "}
            page
          </p>
        </div>
      ) : (
        <div className="space-y-6">
          {/* Active Tasks */}
          {filteredTasks.length > 0 && (
            <div>
              <h2 className="mb-3 text-xs font-medium uppercase tracking-wider text-white/40">
                Active Tasks ({filteredTasks.length})
              </h2>
              <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] overflow-hidden">
                <table className="w-full">
                  <thead>
                    <tr className="border-b border-white/[0.04]">
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Status
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Task
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Model
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Iterations
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Actions
                      </th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-white/[0.04]">
                    {filteredTasks.map((task) => {
                      const Icon = statusIcons[task.status];
                      const config = statusConfig[task.status];
                      return (
                        <tr
                          key={task.id}
                          className="hover:bg-white/[0.02] transition-colors"
                        >
                          <td className="px-4 py-3">
                            <span
                              className={cn(
                                "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[10px] font-medium",
                                config.bg,
                                config.color
                              )}
                            >
                              <Icon
                                className={cn(
                                  "h-3 w-3",
                                  task.status === "running" && "animate-spin"
                                )}
                              />
                              {task.status}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <p className="max-w-md truncate text-sm text-white/80">
                              {task.task}
                            </p>
                          </td>
                          <td className="px-4 py-3">
                            <span className="text-xs text-white/40 font-mono">
                              {task.model.split("/").pop()}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <span className="text-sm text-white tabular-nums">
                              {task.iterations}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <Link
                              href={`/control?task=${task.id}`}
                              className="inline-flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
                            >
                              View <ArrowRight className="h-3 w-3" />
                            </Link>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* Archived Runs */}
          {filteredRuns.length > 0 && (
            <div>
              <h2 className="mb-3 text-xs font-medium uppercase tracking-wider text-white/40">
                Archived Runs ({filteredRuns.length})
              </h2>
              <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] overflow-hidden">
                <table className="w-full">
                  <thead>
                    <tr className="border-b border-white/[0.04]">
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Status
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Input
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Created
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Cost
                      </th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-white/[0.04]">
                    {filteredRuns.map((run) => {
                      const status = run.status as keyof typeof statusIcons;
                      const Icon = statusIcons[status] || Clock;
                      const config =
                        statusConfig[status] || statusConfig.pending;
                      return (
                        <tr
                          key={run.id}
                          className="hover:bg-white/[0.02] transition-colors"
                        >
                          <td className="px-4 py-3">
                            <span
                              className={cn(
                                "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[10px] font-medium",
                                config.bg,
                                config.color
                              )}
                            >
                              <Icon className="h-3 w-3" />
                              {run.status}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <p className="max-w-md truncate text-sm text-white/80">
                              {run.input_text}
                            </p>
                          </td>
                          <td className="px-4 py-3">
                            <span className="text-xs text-white/40">
                              {new Date(run.created_at).toLocaleString()}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <span className="text-sm text-emerald-400 tabular-nums">
                              ${(run.total_cost_cents / 100).toFixed(2)}
                            </span>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
