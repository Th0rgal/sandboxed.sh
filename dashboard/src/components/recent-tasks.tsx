"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { cn } from "@/lib/utils";
import { listTasks, TaskState } from "@/lib/api";
import {
  ArrowRight,
  CheckCircle,
  XCircle,
  Loader,
  Clock,
  Ban,
} from "lucide-react";

const statusIcons = {
  pending: Clock,
  running: Loader,
  completed: CheckCircle,
  failed: XCircle,
  cancelled: Ban,
};

const statusColors = {
  pending: "text-amber-400",
  running: "text-indigo-400",
  completed: "text-emerald-400",
  failed: "text-red-400",
  cancelled: "text-white/40",
};

export function RecentTasks() {
  const [tasks, setTasks] = useState<TaskState[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const fetchTasks = async () => {
      try {
        const data = await listTasks();
        setTasks(data.slice(0, 5));
      } catch (error) {
        console.error("Failed to fetch tasks:", error);
      } finally {
        setLoading(false);
      }
    };

    fetchTasks();
    const interval = setInterval(fetchTasks, 3000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div>
      <div className="mb-4 flex items-center justify-between">
        <h3 className="text-sm font-medium text-white">Recent Tasks</h3>
        <span className="flex items-center gap-1.5 rounded-md bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-400">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
          LIVE
        </span>
      </div>

      {loading ? (
        <p className="text-xs text-white/40">Loading...</p>
      ) : tasks.length === 0 ? (
        <p className="text-xs text-white/40">No tasks yet</p>
      ) : (
        <div className="space-y-2">
          {tasks.map((task) => {
            const Icon = statusIcons[task.status];
            return (
              <Link
                key={task.id}
                href={`/control?task=${task.id}`}
                className="flex items-center justify-between rounded-lg bg-white/[0.02] hover:bg-white/[0.04] border border-white/[0.04] hover:border-white/[0.08] p-3 transition-colors"
              >
                <div className="flex items-center gap-3">
                  <Icon
                    className={cn(
                      "h-4 w-4",
                      statusColors[task.status],
                      task.status === "running" && "animate-spin"
                    )}
                  />
                  <span className="max-w-[180px] truncate text-sm text-white/80">
                    {task.task}
                  </span>
                </div>
                <ArrowRight className="h-4 w-4 text-white/30" />
              </Link>
            );
          })}
        </div>
      )}

      <Link
        href="/history"
        className="mt-4 flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
      >
        View all <ArrowRight className="h-3 w-3" />
      </Link>
    </div>
  );
}
