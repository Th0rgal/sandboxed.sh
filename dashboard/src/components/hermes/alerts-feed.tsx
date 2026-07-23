"use client";

import { useCallback, useMemo, useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import { Bell, Loader } from "lucide-react";

import { listAlerts, type AlertFeedEntry } from "@/lib/api";
import type { AwaitingKind, MissionStatus } from "@/lib/api/missions";
import { getMissionShortName } from "@/lib/mission-display";
import { getMissionDotColor, statusLabel } from "@/lib/mission-status";
import { stableJsonCompare } from "@/lib/swr-config";
import { RelativeTime } from "@/components/ui/relative-time";
import { cn } from "@/lib/utils";

type FeedFilter = "all" | "needs-you" | "finished";

const FILTER_STATUSES: Record<FeedFilter, string[] | undefined> = {
  all: undefined,
  "needs-you": ["awaiting_user"],
  finished: ["completed", "failed", "interrupted", "blocked", "not_feasible"],
};

const FILTER_LABELS: Record<FeedFilter, string> = {
  all: "All",
  "needs-you": "Needs you",
  finished: "Finished",
};

/**
 * Historical feed of mission status-change alerts — the same events Hermes
 * gets webhooked about, straight from the mission_events source of truth.
 */
export function AlertsFeed() {
  const [filter, setFilter] = useState<FeedFilter>("all");
  // Older pages fetched via "load more"; reset when the filter changes.
  const [olderPages, setOlderPages] = useState<AlertFeedEntry[]>([]);
  // `undefined` means no older page has been requested yet. `null` means the
  // server explicitly reported that pagination is exhausted.
  const [olderCursor, setOlderCursor] = useState<string | null | undefined>(
    undefined,
  );
  // First-page boundary for which `olderCursor` was obtained. If SWR later
  // refreshes to a different boundary, page the new gap before trusting the
  // previously exhausted/deeper cursor again.
  const [paginationAnchor, setPaginationAnchor] = useState<
    string | null | undefined
  >(undefined);
  const [loadingMore, setLoadingMore] = useState(false);

  const { data, isLoading } = useSWR(
    ["hermes-alerts", filter],
    () => listAlerts({ statuses: FILTER_STATUSES[filter], limit: 30 }),
    {
      refreshInterval: 15000,
      revalidateOnFocus: false,
      compare: stableJsonCompare,
    },
  );

  const firstPage = useMemo(() => data?.alerts ?? [], [data]);
  // Drop duplicates and keep refreshed gap pages in chronological order even
  // when they were fetched after an already-loaded older tail.
  const entries = useMemo(() => {
    if (olderPages.length === 0) return firstPage;
    const seen = new Set<string>();
    return [...firstPage, ...olderPages]
      .filter((alert) => {
        const key = `${alert.mission_id}:${alert.timestamp}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      })
      .sort(
        (left, right) =>
          Date.parse(right.timestamp) - Date.parse(left.timestamp),
      );
  }, [firstPage, olderPages]);

  const firstPageCursor = data?.next_cursor ?? null;
  const cursor =
    olderCursor === undefined || paginationAnchor !== firstPageCursor
      ? firstPageCursor
      : olderCursor;

  const changeFilter = useCallback((f: FeedFilter) => {
    setFilter(f);
    setOlderPages([]);
    setOlderCursor(undefined);
    setPaginationAnchor(undefined);
  }, []);

  const loadMore = useCallback(async () => {
    if (!cursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await listAlerts({
        statuses: FILTER_STATUSES[filter],
        before: cursor,
        limit: 30,
      });
      setOlderPages((prev) => [...prev, ...page.alerts]);
      setOlderCursor(page.next_cursor);
      setPaginationAnchor(firstPageCursor);
    } finally {
      setLoadingMore(false);
    }
  }, [cursor, filter, firstPageCursor, loadingMore]);

  return (
    <div className="flex h-full min-h-0 flex-col rounded-xl border border-border bg-[rgb(var(--foreground)/0.025)] p-3">
      <div className="mb-2 flex h-6 flex-shrink-0 items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-medium text-[rgb(var(--foreground)/0.75)]">
          <Bell className="h-3.5 w-3.5 text-primary" />
          Updates
        </div>
        <div className="flex gap-0.5">
          {(Object.keys(FILTER_LABELS) as FeedFilter[]).map((f) => (
            <button
              key={f}
              type="button"
              onClick={() => changeFilter(f)}
              className={cn(
                "rounded-md px-1.5 py-0.5 text-[10px] font-medium transition-colors",
                filter === f
                  ? "bg-[rgb(var(--foreground)/0.08)] text-[rgb(var(--foreground)/0.85)]"
                  : "text-[rgb(var(--foreground)/0.4)] hover:text-[rgb(var(--foreground)/0.7)]",
              )}
            >
              {FILTER_LABELS[f]}
            </button>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {isLoading && entries.length === 0 && (
          <div className="flex items-center gap-2 py-4 text-xs text-white/40">
            <Loader className="h-3.5 w-3.5 animate-spin" /> Loading updates…
          </div>
        )}
        {!isLoading && entries.length === 0 && (
          <p className="py-4 text-xs text-white/35">
            No status updates yet. Mission completions, failures and questions
            will land here.
          </p>
        )}

        {entries.map((a) => (
          <AlertRow key={`${a.mission_id}:${a.timestamp}`} alert={a} />
        ))}

        {cursor && (
          <button
            type="button"
            onClick={() => void loadMore()}
            disabled={loadingMore}
            className="mt-2 w-full rounded-lg border border-white/[0.06] py-1.5 text-[11px] text-white/45 transition-colors hover:bg-white/[0.04] hover:text-white/70 disabled:opacity-50"
          >
            {loadingMore ? "Loading…" : "Load older"}
          </button>
        )}
      </div>
    </div>
  );
}

function AlertRow({ alert }: { alert: AlertFeedEntry }) {
  const status = alert.status as MissionStatus;
  const awaitingKind = (alert.mission?.awaiting_kind ?? null) as
    | AwaitingKind
    | null;
  const title =
    alert.mission?.title?.trim() || getMissionShortName(alert.mission_id);
  const ts = new Date(alert.timestamp);

  return (
    <Link
      href={`/control?mission=${alert.mission_id}`}
      className="block border-t border-white/[0.04] px-1 py-2 transition-colors first:border-t-0 hover:bg-white/[0.03]"
    >
      <div className="flex items-center gap-2">
        <span
          className={cn(
            "h-1.5 w-1.5 shrink-0 rounded-full",
            getMissionDotColor(status, false),
          )}
        />
        <span className="min-w-0 flex-1 truncate text-xs text-white/80">
          {title}
        </span>
        {!Number.isNaN(ts.getTime()) && (
          <RelativeTime date={ts} className="shrink-0 text-[10px] text-white/30" />
        )}
      </div>
      <div className="mt-0.5 flex items-center gap-2 pl-3.5">
        <span className="shrink-0 text-[10px] font-medium text-white/50">
          {statusLabel(status, awaitingKind)}
        </span>
        {alert.summary && (
          <span className="min-w-0 truncate text-[11px] text-white/40">
            {alert.summary}
          </span>
        )}
      </div>
      {(alert.delivery || alert.mission?.workspace_name) && (
        <div className="mt-0.5 flex items-center gap-2 pl-3.5 text-[10px] text-white/30">
          {alert.mission?.workspace_name && (
            <span className="truncate">{alert.mission.workspace_name}</span>
          )}
          {alert.delivery && (
            <span
              className={cn(
                "shrink-0",
                alert.delivery.last_error
                  ? "text-red-400/70"
                  : alert.delivery.acknowledged_at
                    ? "text-emerald-400/70"
                    : undefined,
              )}
              title={alert.delivery.last_error ?? undefined}
            >
              telegram{" "}
              {alert.delivery.last_error
                ? "error"
                : alert.delivery.acknowledged_at
                  ? "acked"
                  : alert.delivery.status}
            </span>
          )}
        </div>
      )}
    </Link>
  );
}
