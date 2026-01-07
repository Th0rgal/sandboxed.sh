"use client";

import { useEffect, useState, useRef, useMemo, useCallback } from "react";
import Link from "next/link";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { listMissions, getMissionTree, deleteMission, cleanupEmptyMissions, Mission } from "@/lib/api";
import { ShimmerTableRow } from "@/components/ui/shimmer";
import { CopyButton } from "@/components/ui/copy-button";
import { RelativeTime } from "@/components/ui/relative-time";
import { AgentTreeCanvas, type AgentNode } from "@/components/agent-tree";
import {
  CheckCircle,
  XCircle,
  Clock,
  Loader,
  Ban,
  ArrowRight,
  Search,
  MessageSquare,
  Target,
  ArrowUpDown,
  ArrowUp,
  ArrowDown,
  Network,
  X,
  Trash2,
  Sparkles,
} from "lucide-react";

const statusIcons: Record<string, typeof Clock> = {
  pending: Clock,
  running: Loader,
  completed: CheckCircle,
  failed: XCircle,
  cancelled: Ban,
  active: Clock,
  interrupted: Ban,
  blocked: Ban,
  not_feasible: XCircle,
};

const statusConfig: Record<string, { color: string; bg: string }> = {
  pending: { color: "text-amber-400", bg: "bg-amber-500/10" },
  running: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  completed: { color: "text-emerald-400", bg: "bg-emerald-500/10" },
  failed: { color: "text-red-400", bg: "bg-red-500/10" },
  cancelled: { color: "text-white/40", bg: "bg-white/[0.04]" },
  active: { color: "text-indigo-400", bg: "bg-indigo-500/10" },
  interrupted: { color: "text-amber-400", bg: "bg-amber-500/10" },
  blocked: { color: "text-orange-400", bg: "bg-orange-500/10" },
  not_feasible: { color: "text-rose-400", bg: "bg-rose-500/10" },
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
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

// Convert backend tree node to frontend AgentNode
function convertTreeNode(node: Record<string, unknown>): AgentNode {
  const children = (node["children"] as Record<string, unknown>[] | undefined) ?? [];
  return {
    id: String(node["id"] ?? ""),
    type: String(node["node_type"] ?? "Node") as AgentNode["type"],
    status: String(node["status"] ?? "pending") as AgentNode["status"],
    name: String(node["name"] ?? ""),
    description: String(node["description"] ?? ""),
    model: node["selected_model"] != null ? String(node["selected_model"]) : undefined,
    budgetAllocated: Number(node["budget_allocated"] ?? 0),
    budgetSpent: Number(node["budget_spent"] ?? 0),
    complexity: node["complexity"] != null ? Number(node["complexity"]) : undefined,
    children: children.map((c) => convertTreeNode(c)),
  };
}

export default function HistoryPage() {
  const [missions, setMissions] = useState<Mission[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState<string>("all");
  const [search, setSearch] = useState("");
  const [sortField, setSortField] = useState<SortField>("date");
  const [sortDirection, setSortDirection] = useState<SortDirection>("desc");
  const fetchedRef = useRef(false);

  // Tree preview state
  const [previewMissionId, setPreviewMissionId] = useState<string | null>(null);
  const [previewTree, setPreviewTree] = useState<AgentNode | null>(null);
  const [loadingTree, setLoadingTree] = useState(false);

  // Track the mission ID being fetched to prevent race conditions
  const fetchingTreeMissionIdRef = useRef<string | null>(null);

  // Delete state
  const [deletingMissionId, setDeletingMissionId] = useState<string | null>(null);
  const [cleaningUp, setCleaningUp] = useState(false);

  useEffect(() => {
    if (fetchedRef.current) return;
    fetchedRef.current = true;

    const fetchData = async () => {
      try {
        const missionsData = await listMissions().catch(() => []);
        setMissions(missionsData);
      } catch (error) {
        console.error("Failed to fetch data:", error);
        toast.error("Failed to load history");
      } finally {
        setLoading(false);
      }
    };

    fetchData();
  }, []);

  // Handle Escape key for modal
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && previewMissionId) {
        setPreviewMissionId(null);
        setPreviewTree(null);
        fetchingTreeMissionIdRef.current = null;
      }
    };
    if (previewMissionId) {
      document.addEventListener("keydown", handleKeyDown);
      return () => document.removeEventListener("keydown", handleKeyDown);
    }
  }, [previewMissionId]);

  // Load tree for preview
  const handlePreviewTree = useCallback(async (missionId: string) => {
    if (previewMissionId === missionId) {
      // Toggle off
      setPreviewMissionId(null);
      setPreviewTree(null);
      fetchingTreeMissionIdRef.current = null;
      return;
    }

    setPreviewMissionId(missionId);
    setLoadingTree(true);
    fetchingTreeMissionIdRef.current = missionId;
    
    try {
      const tree = await getMissionTree(missionId);
      
      // Race condition guard: only update if this is still the mission we want
      if (fetchingTreeMissionIdRef.current !== missionId) {
        return; // Another mission was requested, discard this response
      }
      
      if (tree && isRecord(tree)) {
        setPreviewTree(convertTreeNode(tree as Record<string, unknown>));
      } else {
        setPreviewTree(null);
        toast.error("No tree data available for this mission");
      }
    } catch {
      // Race condition guard: only update if this is still the mission we want
      if (fetchingTreeMissionIdRef.current !== missionId) {
        return;
      }
      
      setPreviewTree(null);
      toast.error("Failed to load tree");
    } finally {
      // Only clear loading if this is still the current fetch
      if (fetchingTreeMissionIdRef.current === missionId) {
        setLoadingTree(false);
      }
    }
  }, [previewMissionId]);

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
      setMissions(prev => prev.filter(m => m.id !== missionId));
      toast.success("Mission deleted");
    } catch (error) {
      console.error("Failed to delete mission:", error);
      toast.error("Failed to delete mission");
    } finally {
      setDeletingMissionId(null);
    }
  }, [missions]);

  const handleCleanupEmpty = useCallback(async () => {
    setCleaningUp(true);
    try {
      const result = await cleanupEmptyMissions();
      if (result.deleted_count > 0) {
        // Refresh the missions list
        const missionsData = await listMissions().catch(() => []);
        setMissions(missionsData);
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
  }, []);

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
        <h1 className="text-xl font-semibold text-white">Agents</h1>
        <p className="mt-1 text-sm text-white/50">
          Mission history and agent tree visualization
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
                      <ShimmerTableRow columns={5} />
                      <ShimmerTableRow columns={5} />
                      <ShimmerTableRow columns={5} />
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
                      const Icon = statusIcons[mission.status] || Target;
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
                                onClick={() => handlePreviewTree(mission.id)}
                                className={cn(
                                  "inline-flex items-center gap-1 text-xs transition-colors",
                                  previewMissionId === mission.id
                                    ? "text-emerald-400 hover:text-emerald-300"
                                    : "text-white/40 hover:text-white/60"
                                )}
                                title="View agent tree"
                              >
                                <Network className="h-3 w-3" />
                              </button>
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

      {/* Agent Tree Modal */}
      {previewMissionId && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
          onClick={() => {
            setPreviewMissionId(null);
            setPreviewTree(null);
            fetchingTreeMissionIdRef.current = null;
          }}
        >
          <div
            className="relative w-[90vw] h-[85vh] max-w-6xl rounded-2xl bg-[#0a0a0a] border border-white/[0.08] shadow-2xl overflow-hidden flex flex-col"
            onClick={(e) => e.stopPropagation()}
          >
            {/* Modal Header */}
            <div className="flex items-center justify-between px-6 py-4 border-b border-white/[0.06]">
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10">
                  <Network className="h-5 w-5 text-emerald-400" />
                </div>
                <div>
                  <h2 className="text-lg font-semibold text-white">Agent Tree</h2>
                  <p className="text-xs text-white/40">
                    {missions.find((m) => m.id === previewMissionId)?.title?.slice(0, 50) || "Mission visualization"}
                  </p>
                </div>
              </div>
              <button
                onClick={() => {
                  setPreviewMissionId(null);
                  setPreviewTree(null);
                  fetchingTreeMissionIdRef.current = null;
                }}
                className="flex h-10 w-10 items-center justify-center rounded-xl text-white/40 hover:bg-white/[0.04] hover:text-white/70 transition-colors"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {/* Modal Content */}
            <div className="flex-1 min-h-0">
              {loadingTree ? (
                <div className="flex flex-col items-center justify-center h-full">
                  <Loader className="h-8 w-8 animate-spin text-emerald-400 mb-3" />
                  <p className="text-sm text-white/60">Loading agent tree...</p>
                </div>
              ) : previewTree ? (
                <AgentTreeCanvas tree={previewTree} className="w-full h-full" />
              ) : (
                <div className="flex flex-col items-center justify-center h-full text-center p-8">
                  <div className="flex h-20 w-20 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
                    <Network className="h-10 w-10 text-white/20" />
                  </div>
                  <p className="text-lg font-medium text-white/60">No tree data available</p>
                  <p className="text-sm text-white/30 mt-2 max-w-md">
                    Agent tree data is captured during mission execution. 
                    This mission may have been completed before tree tracking was enabled.
                  </p>
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
