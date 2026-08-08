"use client";

import { useState, useMemo, useCallback } from "react";
import Link from "next/link";
import useSWR from "swr";
import { toast } from "@/components/toast";
import { cn } from "@/lib/utils";
import { listMissions, deleteMission, cleanupEmptyMissions } from "@/lib/api";
import { CopyButton } from "@/components/ui/copy-button";
import { RelativeTime } from "@/components/ui/relative-time";
import {
  Loader,
  ArrowRight,
  Search,
  MessageSquare,
  Target,
  ArrowUpDown,
  ArrowUp,
  ArrowDown,
  Trash2,
  Sparkles,
} from "lucide-react";
import { getStatusIcon } from "@/components/ui/status-icons";

const statusConfig: Record<string, { color: string; bg: string }> = {
  pending: { color: "text-amber-400", bg: "bg-amber-500/10" },
  running: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  completed: { color: "text-emerald-400", bg: "bg-emerald-500/10" },
  failed: { color: "text-red-400", bg: "bg-red-500/10" },
  cancelled: { color: "text-white/40", bg: "bg-white/[0.04]" },
  active: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  waiting_background: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  interrupted: { color: "text-amber-400", bg: "bg-amber-500/10" },
  blocked: { color: "text-orange-400", bg: "bg-orange-500/10" },
  not_feasible: { color: "text-rose-400", bg: "bg-rose-500/10" },
};

// Cell shapes mirror the real missions table:
// 1) status pill (icon + label)  2) icon + truncated title
// 3) short numeric count          4) short relative-time text
// 5) action button cluster
function HistoryTableRowSkeleton() {
  return (
    <tr className="animate-pulse">
      <td className="px-4 py-3">
        <div className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 bg-white/[0.04]">
          <div className="h-3 w-3 rounded-sm bg-white/[0.08]" />
          <div className="h-3 w-14 rounded bg-white/[0.08]" />
        </div>
      </td>
      <td className="px-4 py-3">
        <div className="flex items-center gap-2">
          <div className="h-4 w-4 rounded bg-indigo-500/20 shrink-0" />
          <div className="h-4 w-64 max-w-md rounded bg-white/[0.06]" />
        </div>
      </td>
      <td className="px-4 py-3">
        <div className="h-4 w-8 rounded bg-white/[0.06]" />
      </td>
      <td className="px-4 py-3">
        <div className="h-3 w-20 rounded bg-white/[0.04]" />
      </td>
      <td className="px-4 py-3">
        <div className="flex items-center gap-2">
          <div className="h-4 w-16 rounded bg-indigo-500/20" />
          <div className="h-4 w-4 rounded bg-white/[0.04]" />
        </div>
      </td>
    </tr>
  );
}

type SortField = 'date' | 'status' | 'messages';
type SortDirection = 'asc' | 'desc';

function SortButton({ 
  field, 
  currentField, 
  direction, 
  onClick 
}: { 
  field: SortField;
  currentField: SortField;
  direction: SortDirection;
  onClick: () => void;
}) {
  const isActive = field === currentField;
  
  return (
    <button
      onClick={onClick}
      className={cn(
        "ml-1 p-0.5 rounded transition-colors",
        isActive ? "text-white/60" : "text-white/20 hover:text-white/40"
      )}
    >
      {isActive ? (
        direction === 'asc' ? <ArrowUp className="h-3 w-3" /> : <ArrowDown className="h-3 w-3" />
      ) : (
        <ArrowUpDown className="h-3 w-3" />
      )}
    </button>
  );
}

export default function HistoryPage() {
  const [filter, setFilter] = useState<string>("all");
  const [search, setSearch] = useState("");
  const [sortField, setSortField] = useState<SortField>("date");
  const [sortDirection, setSortDirection] = useState<SortDirection>("desc");

  // SWR: fetch missions (shared "missions" key with other views)
  const { data: missions = [], isLoading: loading, mutate: mutateMissions } = useSWR(
    'missions',
    listMissions,
    { revalidateOnFocus: false }
  );

  // Delete state
  const [deletingMissionId, setDeletingMissionId] = useState<string | null>(null);
  const [cleaningUp, setCleaningUp] = useState(false);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(sortDirection === "asc" ? "desc" : "asc");
    } else {
      setSortField(field);
      setSortDirection("desc");
    }
  };

  const handleDeleteMission = useCallback(async (missionId: string, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();

    const mission = missions.find(m => m.id === missionId);
    if (mission?.status === "active") {
      toast.error("Cannot delete an active mission");
      return;
    }

    setDeletingMissionId(missionId);
    try {
      await deleteMission(missionId);
      // Optimistic update: filter out deleted mission from cache
      mutateMissions(missions.filter(m => m.id !== missionId), false);
      toast.success("Mission deleted");
    } catch (error) {
      console.error("Failed to delete mission:", error);
      toast.error("Failed to delete mission");
    } finally {
      setDeletingMissionId(null);
    }
  }, [missions, mutateMissions]);

  const handleCleanupEmpty = useCallback(async () => {
    setCleaningUp(true);
    try {
      const result = await cleanupEmptyMissions();
      if (result.deleted_count > 0) {
        // Refresh the missions list from server
        await mutateMissions();
        toast.success(`Cleaned up ${result.deleted_count} empty mission${result.deleted_count === 1 ? '' : 's'}`);
      } else {
        toast.info("No empty missions to clean up");
      }
    } catch (error) {
      console.error("Failed to cleanup missions:", error);
      toast.error("Failed to cleanup missions");
    } finally {
      setCleaningUp(false);
    }
  }, [mutateMissions]);

  const filteredMissions = useMemo(() => {
    const filtered = missions.filter((mission) => {
      if (filter !== "all" && mission.status !== filter) return false;
      const title = mission.title || "";
      if (search && !title.toLowerCase().includes(search.toLowerCase()))
        return false;
      return true;
    });

    // Sort missions
    return filtered.sort((a, b) => {
      let comparison = 0;
      switch (sortField) {
        case "date":
          comparison =
            new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
          break;
        case "status":
          comparison = a.status.localeCompare(b.status);
          break;
        case "messages":
          comparison = b.history.length - a.history.length;
          break;
      }
      return sortDirection === "asc" ? -comparison : comparison;
    });
  }, [missions, filter, search, sortField, sortDirection]);

  const hasData = filteredMissions.length > 0;

  return (
    <div className="p-6">
      {/* Header */}
      <div className="mb-6">
        <h1 className="text-xl font-semibold text-white">Missions</h1>
        <p className="mt-1 text-sm text-white/50">
          Mission history and subagent tree visualization
        </p>
      </div>

      {/* Filters */}
      <div className="mb-6 flex items-center gap-4">
        <div className="relative flex-1 max-w-md">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
          <input
            type="text"
            placeholder="Search missions..."
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

        <button
          onClick={handleCleanupEmpty}
          disabled={cleaningUp}
          className={cn(
            "inline-flex items-center gap-2 px-3 py-2 rounded-lg text-xs font-medium transition-colors",
            "bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04]",
            "text-white/60 hover:text-white/80",
            cleaningUp && "opacity-50 cursor-not-allowed"
          )}
        >
          {cleaningUp ? (
            <Loader className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Sparkles className="h-3.5 w-3.5" />
          )}
          Cleanup Empty
        </button>
      </div>

      {/* Content */}
      <div>
          {loading ? (
            <div className="space-y-6">
              {/* Shimmer for missions table */}
              <div>
                <div className="h-4 w-24 bg-white/[0.04] rounded mb-3 animate-pulse" />
                <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] overflow-hidden">
                  <table className="w-full">
                    <thead>
                      <tr className="border-b border-white/[0.04]">
                        <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                          Status
                        </th>
                        <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                          Mission
                        </th>
                        <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                          Messages
                        </th>
                        <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                          Updated
                        </th>
                        <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                          Actions
                        </th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-white/[0.04]">
                      <HistoryTableRowSkeleton />
                      <HistoryTableRowSkeleton />
                      <HistoryTableRowSkeleton />
                    </tbody>
                  </table>
                </div>
              </div>
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
              {/* Missions */}
              <div>
              <h2 className="mb-3 text-xs font-medium uppercase tracking-wider text-white/40">
                Missions ({filteredMissions.length})
              </h2>
              <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] overflow-hidden">
                <table className="w-full">
                  <thead>
                    <tr className="border-b border-white/[0.04]">
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        <span className="flex items-center">
                          Status
                          <SortButton field="status" currentField={sortField} direction={sortDirection} onClick={() => handleSort("status")} />
                        </span>
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Mission
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        <span className="flex items-center">
                          Messages
                          <SortButton field="messages" currentField={sortField} direction={sortDirection} onClick={() => handleSort("messages")} />
                        </span>
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        <span className="flex items-center">
                          Updated
                          <SortButton field="date" currentField={sortField} direction={sortDirection} onClick={() => handleSort("date")} />
                        </span>
                      </th>
                      <th className="px-4 py-3 text-left text-[10px] font-medium uppercase tracking-wider text-white/40">
                        Actions
                      </th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-white/[0.04]">
                    {filteredMissions.map((mission) => {
                      const Icon = getStatusIcon(mission.status, Target);
                      const config = statusConfig[mission.status] || statusConfig.active;
                      const title = mission.title || "Untitled Mission";
                      const displayTitle = title.length > 80 ? title.slice(0, 80) + "..." : title;
                      return (
                        <tr
                          key={mission.id}
                          className="group hover:bg-white/[0.02] transition-colors"
                        >
                          <td className="px-4 py-3">
                            <span
                              className={cn(
                                "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[10px] font-medium capitalize",
                                config.bg,
                                config.color
                              )}
                            >
                              <Icon className="h-3 w-3" />
                              {mission.status}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <div className="flex items-center gap-2">
                              <Target className="h-4 w-4 text-indigo-400 shrink-0" />
                              <p className="max-w-md truncate text-sm text-white/80">
                                {displayTitle}
                              </p>
                              <CopyButton text={title} showOnHover label="Copied title" />
                            </div>
                          </td>
                          <td className="px-4 py-3">
                            <span className="text-sm text-white/60 tabular-nums">
                              {mission.history.length}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            <RelativeTime 
                              date={mission.updated_at} 
                              className="text-xs text-white/40"
                            />
                          </td>
                          <td className="px-4 py-3">
                            <div className="flex items-center gap-2">
                              <Link
                                href={`/control?mission=${mission.id}`}
                                className="inline-flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
                              >
                                {mission.status === "active" ? "Continue" : "View"}{" "}
                                <ArrowRight className="h-3 w-3" />
                              </Link>
                              <button
                                onClick={(e) => handleDeleteMission(mission.id, e)}
                                disabled={deletingMissionId === mission.id || mission.status === "active"}
                                className={cn(
                                  "inline-flex items-center gap-1 text-xs transition-colors opacity-0 group-hover:opacity-100",
                                  deletingMissionId === mission.id
                                    ? "text-white/30 cursor-not-allowed"
                                    : mission.status === "active"
                                    ? "text-white/20 cursor-not-allowed"
                                    : "text-white/40 hover:text-red-400"
                                )}
                                title={mission.status === "active" ? "Cannot delete active mission" : "Delete mission"}
                              >
                                {deletingMissionId === mission.id ? (
                                  <Loader className="h-3 w-3 animate-spin" />
                                ) : (
                                  <Trash2 className="h-3 w-3" />
                                )}
                              </button>
                              <CopyButton
                                text={mission.id}
                                showOnHover
                                label="Copied mission ID"
                                className="opacity-0 group-hover:opacity-100"
                              />
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
            </div>
          )}
      </div>
    </div>
  );
}
