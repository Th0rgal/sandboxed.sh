"use client";

import { useEffect, useState, useMemo } from "react";
import { toast } from "@/components/toast";
import {
  listMissions,
  listRuns,
  getStats,
  type Mission,
  type Run,
} from "@/lib/api";
import { formatCents } from "@/lib/utils";
import {
  TrendingUp,
  DollarSign,
  Activity,
  CheckCircle,
  XCircle,
  BarChart3,
  PieChart,
  Calendar,
  Zap,
} from "lucide-react";

interface CostByDay {
  date: string;
  cost: number;
  missions: number;
}

interface StatusBreakdown {
  status: string;
  count: number;
  color: string;
}

function MetricSkeleton({ className }: { className?: string }) {
  return <div className={`animate-pulse rounded bg-white/[0.06] ${className ?? ""}`} />;
}

export default function AnalyticsPage() {
  const [missions, setMissions] = useState<Mission[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [totalCostCents, setTotalCostCents] = useState(0);
  const [actualCostCents, setActualCostCents] = useState(0);
  const [estimatedCostCents, setEstimatedCostCents] = useState(0);
  const [unknownCostCents, setUnknownCostCents] = useState(0);
  const [loading, setLoading] = useState(true);
  const [timeRange, setTimeRange] = useState<"7d" | "30d" | "all">("7d");

  // Compute ISO-8601 lower bound from the selected time range
  const sinceDate = useMemo(() => {
    if (timeRange === "all") return undefined;
    const days = timeRange === "7d" ? 7 : 30;
    return new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString();
  }, [timeRange]);

  // Fetch missions, runs, and all-time stats once on mount
  useEffect(() => {
    async function fetchData() {
      try {
        const [missionsData, runsData, allTimeStats] = await Promise.all([
          listMissions(),
          listRuns(100, 0),
          getStats(),
        ]);
        setMissions(missionsData);
        setRuns(runsData.runs);
        setTotalCostCents(allTimeStats.total_cost_cents);
      } catch (err) {
        console.error("Failed to fetch analytics:", err);
        toast.error("Failed to load analytics");
      } finally {
        setLoading(false);
      }
    }
    fetchData();
  }, []);

  // Re-fetch cost breakdown whenever the time range changes
  useEffect(() => {
    async function fetchPeriodStats() {
      try {
        const stats = await getStats(sinceDate);
        setActualCostCents(stats.actual_cost_cents ?? 0);
        setEstimatedCostCents(stats.estimated_cost_cents ?? 0);
        setUnknownCostCents(stats.unknown_cost_cents ?? 0);
      } catch {
        // Silently fall back — the all-time total is still visible
      }
    }
    fetchPeriodStats();
  }, [sinceDate]);

  // Calculate cost by day
  const costByDay = useMemo((): CostByDay[] => {
    const now = new Date();
    const days = timeRange === "7d" ? 7 : timeRange === "30d" ? 30 : 90;
    const startDate = new Date(now.getTime() - days * 24 * 60 * 60 * 1000);

    const byDay: Record<string, { cost: number; missions: number }> = {};

    // Initialize all days with 0
    for (let i = 0; i < days; i++) {
      const date = new Date(startDate.getTime() + i * 24 * 60 * 60 * 1000);
      const dateStr = date.toISOString().split("T")[0];
      byDay[dateStr] = { cost: 0, missions: 0 };
    }

    // Aggregate runs by day
    runs.forEach((run) => {
      const date = new Date(run.created_at).toISOString().split("T")[0];
      if (byDay[date]) {
        byDay[date].cost += run.total_cost_cents;
        byDay[date].missions += 1;
      }
    });

    return Object.entries(byDay)
      .map(([date, data]) => ({
        date,
        ...data,
      }))
      .sort((a, b) => a.date.localeCompare(b.date));
  }, [runs, timeRange]);

  // Calculate status breakdown
  const statusBreakdown = useMemo((): StatusBreakdown[] => {
    const counts: Record<string, number> = {};
    missions.forEach((m) => {
      counts[m.status] = (counts[m.status] || 0) + 1;
    });

    const colors: Record<string, string> = {
      active: "bg-indigo-500",
      completed: "bg-emerald-500",
      failed: "bg-red-500",
      interrupted: "bg-amber-500",
      blocked: "bg-orange-500",
      not_feasible: "bg-rose-500",
    };

    return Object.entries(counts).map(([status, count]) => ({
      status,
      count,
      color: colors[status] || "bg-gray-500",
    }));
  }, [missions]);

  // Calculate average cost per mission
  const avgCostPerMission = useMemo(() => {
    if (runs.length === 0) return 0;
    const totalCost = runs.reduce((sum, run) => sum + run.total_cost_cents, 0);
    return totalCost / runs.length;
  }, [runs]);

  // Calculate mission stats from actual mission data
  const missionStats = useMemo(() => {
    const completed = missions.filter(m => m.status === "completed").length;
    const failed = missions.filter(m => m.status === "failed" || m.status === "not_feasible").length;
    const finished = completed + failed;
    const successRate = finished > 0 ? completed / finished : 1;
    // Use totalCostCents from stats API (includes ALL runs, not just first 100)
    return { completed, failed, successRate, totalCost: totalCostCents };
  }, [missions, totalCostCents]);

  // Calculate max single day cost
  const maxDayCost = useMemo(() => {
    return Math.max(...costByDay.map((d) => d.cost), 1);
  }, [costByDay]);

  // Calculate total period cost
  const periodTotalCost = useMemo(() => {
    return costByDay.reduce((sum, d) => sum + d.cost, 0);
  }, [costByDay]);

  return (
    <div className="min-h-screen p-6">
      {/* Header */}
      <div className="mb-6">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-xl font-semibold text-white flex items-center gap-2">
              <BarChart3 className="h-5 w-5 text-indigo-400" />
              Analytics
            </h1>
            <p className="mt-1 text-sm text-white/50">
              Mission costs and performance metrics
            </p>
          </div>

          {/* Time range selector */}
          <div className="flex items-center gap-1 bg-white/[0.02] border border-white/[0.06] rounded-lg p-1">
            {(["7d", "30d", "all"] as const).map((range) => (
              <button
                key={range}
                onClick={() => setTimeRange(range)}
                className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                  timeRange === range
                    ? "bg-indigo-500/20 text-indigo-400"
                    : "text-white/50 hover:text-white/70"
                }`}
              >
                {range === "7d"
                  ? "7 Days"
                  : range === "30d"
                  ? "30 Days"
                  : "All Time"}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Summary Cards */}
      <div className="grid grid-cols-4 gap-4 mb-6">
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <DollarSign className="h-4 w-4 text-emerald-400" />
            <span className="text-xs text-white/50">Total Spent</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {loading ? <MetricSkeleton className="h-8 w-28" /> : formatCents(missionStats.totalCost)}
          </div>
          <div className="text-xs text-white/40 mt-1">
            {loading ? <MetricSkeleton className="h-3 w-36" /> : `${formatCents(periodTotalCost)} in selected period`}
          </div>
          {/* Cost source breakdown */}
          {(actualCostCents > 0 || estimatedCostCents > 0 || unknownCostCents > 0) && (
            <div className="mt-2 pt-2 border-t border-white/[0.06] space-y-1">
              {actualCostCents > 0 && (
                <div className="flex items-center justify-between text-xs">
                  <span className="text-emerald-400/70">Actual</span>
                  <span className="font-mono text-emerald-400/70">{formatCents(actualCostCents)}</span>
                </div>
              )}
              {estimatedCostCents > 0 && (
                <div className="flex items-center justify-between text-xs">
                  <span className="text-amber-300/70">Estimated</span>
                  <span className="font-mono text-amber-300/70">{formatCents(estimatedCostCents)}</span>
                </div>
              )}
              {unknownCostCents > 0 && (
                <div className="flex items-center justify-between text-xs">
                  <span className="text-white/30">Unknown</span>
                  <span className="font-mono text-white/30">{formatCents(unknownCostCents)}</span>
                </div>
              )}
            </div>
          )}
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <Activity className="h-4 w-4 text-indigo-400" />
            <span className="text-xs text-white/50">Total Missions</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {loading ? <MetricSkeleton className="h-8 w-12" /> : missions.length}
          </div>
          <div className="text-xs text-white/40 mt-1">
            {loading ? <MetricSkeleton className="h-3 w-20" /> : `${runs.length} runs total`}
          </div>
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <TrendingUp className="h-4 w-4 text-amber-400" />
            <span className="text-xs text-white/50">Avg Cost/Mission</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {loading ? <MetricSkeleton className="h-8 w-24" /> : formatCents(Math.round(avgCostPerMission))}
          </div>
          <div className="text-xs text-white/40 mt-1">
            per completed run
          </div>
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <CheckCircle className="h-4 w-4 text-emerald-400" />
            <span className="text-xs text-white/50">Mission Success Rate</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {loading ? <MetricSkeleton className="h-8 w-16" /> : `${(missionStats.successRate * 100).toFixed(0)}%`}
          </div>
          <div className="text-xs text-white/40 mt-1">
            {loading ? <MetricSkeleton className="h-3 w-40" /> : `${missionStats.completed} missions completed, ${missionStats.failed} failed`}
          </div>
        </div>
      </div>

      {/* Charts Row */}
      <div className="grid grid-cols-3 gap-6 mb-6">
        {/* Cost Over Time Chart */}
        <div className="col-span-2 bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center justify-between mb-4">
            <h2 className="text-sm font-medium text-white flex items-center gap-2">
              <Calendar className="h-4 w-4 text-white/50" />
              Cost Over Time
            </h2>
          </div>

          {/* Simple bar chart */}
          {loading ? (
            <div className="h-48 flex items-end gap-1 animate-pulse">
              {Array.from({ length: 14 }).map((_, idx) => (
                <div
                  key={idx}
                  className="flex-1 rounded-t bg-white/[0.06]"
                  style={{ height: `${25 + (idx % 5) * 12}%` }}
                />
              ))}
            </div>
          ) : (
            <div className="h-48 flex items-end gap-1">
              {costByDay.slice(-14).map((day) => {
                const height = maxDayCost > 0 ? (day.cost / maxDayCost) * 100 : 0;
                const date = new Date(day.date);
                const isWeekend = date.getDay() === 0 || date.getDay() === 6;

                return (
                  <div
                    key={day.date}
                    className="flex-1 flex flex-col items-center gap-1"
                  >
                    <div className="relative w-full flex flex-col items-center">
                      {day.cost > 0 && (
                        <span className="text-[9px] text-white/40 mb-1">
                          {formatCents(day.cost)}
                        </span>
                      )}
                      <div
                        className={`w-full rounded-t transition-all ${
                          isWeekend ? "bg-indigo-500/30" : "bg-indigo-500/50"
                        } hover:bg-indigo-500/70`}
                        style={{ height: `${Math.max(height, 2)}%` }}
                        title={`${day.date}: ${formatCents(day.cost)} (${day.missions} missions)`}
                      />
                    </div>
                    <span className="text-[9px] text-white/30">
                      {date.getDate()}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Status Breakdown */}
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <h2 className="text-sm font-medium text-white flex items-center gap-2 mb-4">
            <PieChart className="h-4 w-4 text-white/50" />
            Mission Status
          </h2>

          <div className="space-y-3">
            {loading ? (
              Array.from({ length: 5 }).map((_, idx) => (
                <div key={idx} className="animate-pulse">
                  <div className="mb-1 flex items-center justify-between">
                    <MetricSkeleton className="h-3 w-20" />
                    <MetricSkeleton className="h-3 w-12" />
                  </div>
                  <MetricSkeleton className="h-2 w-full" />
                </div>
              ))
            ) : statusBreakdown.length > 0 ? (
              statusBreakdown.map((item) => {
                const percentage = (item.count / missions.length) * 100;
                return (
                  <div key={item.status}>
                    <div className="flex items-center justify-between text-xs mb-1">
                      <span className="text-white/70 capitalize">
                        {item.status.replace("_", " ")}
                      </span>
                      <span className="text-white/50">
                        {item.count} ({percentage.toFixed(0)}%)
                      </span>
                    </div>
                    <div className="h-2 bg-white/[0.04] rounded-full overflow-hidden">
                      <div
                        className={`h-full ${item.color} rounded-full transition-all`}
                        style={{ width: `${percentage}%` }}
                      />
                    </div>
                  </div>
                );
              })
            ) : (
              <p className="text-white/40 text-sm text-center py-4">
                No missions yet
              </p>
            )}
          </div>
        </div>
      </div>

      {/* Recent Expensive Runs */}
      <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
        <h2 className="text-sm font-medium text-white flex items-center gap-2 mb-4">
          <Zap className="h-4 w-4 text-amber-400" />
          Most Expensive Runs
        </h2>

        <div className="overflow-x-auto">
          <table className="w-full">
            <thead>
              <tr className="text-left text-xs text-white/40 border-b border-white/[0.06]">
                <th className="pb-2 font-medium">Run ID</th>
                <th className="pb-2 font-medium">Task</th>
                <th className="pb-2 font-medium">Status</th>
                <th className="pb-2 font-medium text-right">Cost</th>
                <th className="pb-2 font-medium text-right">Date</th>
              </tr>
            </thead>
            <tbody className="text-sm">
              {runs
                .sort((a, b) => b.total_cost_cents - a.total_cost_cents)
                .slice(0, 10)
                .map((run) => (
                  <tr
                    key={run.id}
                    className="border-b border-white/[0.04] hover:bg-white/[0.02]"
                  >
                    <td className="py-2 font-mono text-xs text-white/50">
                      {run.id.slice(0, 8)}...
                    </td>
                    <td className="py-2 text-white/70 max-w-xs truncate">
                      {run.input_text?.slice(0, 50) || "—"}...
                    </td>
                    <td className="py-2">
                      <span
                        className={`inline-flex items-center gap-1 text-xs ${
                          run.status === "completed"
                            ? "text-emerald-400"
                            : run.status === "failed"
                            ? "text-red-400"
                            : "text-amber-400"
                        }`}
                      >
                        {run.status === "completed" ? (
                          <CheckCircle className="h-3 w-3" />
                        ) : (
                          <XCircle className="h-3 w-3" />
                        )}
                        {run.status}
                      </span>
                    </td>
                    <td className="py-2 text-right font-mono text-emerald-400">
                      {formatCents(run.total_cost_cents)}
                    </td>
                    <td className="py-2 text-right text-white/40">
                      {new Date(run.created_at).toLocaleDateString()}
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>

          {runs.length === 0 && (
            <p className="text-white/40 text-sm text-center py-8">
              No runs yet
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
