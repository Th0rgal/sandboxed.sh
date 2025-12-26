"use client";

import { useEffect, useState, useMemo } from "react";
import { toast } from "sonner";
import {
  getStats,
  listMissions,
  listRuns,
  type StatsResponse,
  type Mission,
  type Run,
} from "@/lib/api";
import { formatCents } from "@/lib/utils";
import {
  TrendingUp,
  DollarSign,
  Activity,
  Clock,
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

export default function AnalyticsPage() {
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [missions, setMissions] = useState<Mission[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [loading, setLoading] = useState(true);
  const [timeRange, setTimeRange] = useState<"7d" | "30d" | "all">("7d");

  useEffect(() => {
    async function fetchData() {
      try {
        const [statsData, missionsData, runsData] = await Promise.all([
          getStats(),
          listMissions(),
          listRuns(100, 0),
        ]);
        setStats(statsData);
        setMissions(missionsData);
        setRuns(runsData.runs);
      } catch (err) {
        console.error("Failed to fetch analytics:", err);
        toast.error("Failed to load analytics");
      } finally {
        setLoading(false);
      }
    }
    fetchData();
  }, []);

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

  // Calculate max single day cost
  const maxDayCost = useMemo(() => {
    return Math.max(...costByDay.map((d) => d.cost), 1);
  }, [costByDay]);

  // Calculate total period cost
  const periodTotalCost = useMemo(() => {
    return costByDay.reduce((sum, d) => sum + d.cost, 0);
  }, [costByDay]);

  if (loading) {
    return (
      <div className="flex h-screen items-center justify-center">
        <div className="flex items-center gap-3">
          <div className="h-5 w-5 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
          <span className="text-white/60">Loading analytics...</span>
        </div>
      </div>
    );
  }

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
            {formatCents(stats?.total_cost_cents ?? 0)}
          </div>
          <div className="text-xs text-white/40 mt-1">
            {formatCents(periodTotalCost)} in selected period
          </div>
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <Activity className="h-4 w-4 text-indigo-400" />
            <span className="text-xs text-white/50">Total Missions</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {missions.length}
          </div>
          <div className="text-xs text-white/40 mt-1">
            {runs.length} runs total
          </div>
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <TrendingUp className="h-4 w-4 text-amber-400" />
            <span className="text-xs text-white/50">Avg Cost/Mission</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {formatCents(Math.round(avgCostPerMission))}
          </div>
          <div className="text-xs text-white/40 mt-1">
            per completed run
          </div>
        </div>

        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center gap-2 mb-2">
            <CheckCircle className="h-4 w-4 text-emerald-400" />
            <span className="text-xs text-white/50">Success Rate</span>
          </div>
          <div className="text-2xl font-semibold text-white">
            {((stats?.success_rate ?? 1) * 100).toFixed(0)}%
          </div>
          <div className="text-xs text-white/40 mt-1">
            {stats?.completed_tasks ?? 0} completed, {stats?.failed_tasks ?? 0} failed
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
          <div className="h-48 flex items-end gap-1">
            {costByDay.slice(-14).map((day, i) => {
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
        </div>

        {/* Status Breakdown */}
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <h2 className="text-sm font-medium text-white flex items-center gap-2 mb-4">
            <PieChart className="h-4 w-4 text-white/50" />
            Mission Status
          </h2>

          <div className="space-y-3">
            {statusBreakdown.length > 0 ? (
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
