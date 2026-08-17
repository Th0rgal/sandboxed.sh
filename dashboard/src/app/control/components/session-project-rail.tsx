"use client";

import Link from "next/link";
import useSWR from "swr";

import {
  getProject,
  getProjectBySession,
  getProjectsOverview,
} from "@/lib/api/projects";
import {
  cardSummary,
  resolveSessionProjectSlug,
  viewMovingItems,
  viewOpenItems,
  viewPendingDecisions,
} from "@/components/projects/project-card-view";
import { parseMode } from "@/components/projects/project-health";
import { pollingFetchConfig } from "@/lib/swr-config";
import { cn } from "@/lib/utils";

/** Item-first project snapshot docked to the right of a bound Hermes session. */
export function SessionProjectRail({ sessionId }: { sessionId: string }) {
  const { data: resolved } = useSWR(
    ["project-by-session", sessionId],
    async () => {
      try {
        return await getProjectBySession(sessionId);
      } catch {
        const overview = await getProjectsOverview();
        const match = overview.projects.find(
          (project) => project.conversation?.session_id === sessionId,
        );
        return match ? { slug: match.slug, session_id: sessionId } : null;
      }
    },
    { ...pollingFetchConfig, shouldRetryOnError: false },
  );
  const rawSlug = resolved?.slug;
  const { data: overview } = useSWR(
    rawSlug ? "projects-overview" : null,
    getProjectsOverview,
    pollingFetchConfig,
  );
  const slug = resolveSessionProjectSlug({
    resolvedSlug: rawSlug,
    sessionId,
    projects: overview?.projects ?? [],
  });
  const { data: detail } = useSWR(
    slug ? ["project-detail", slug] : null,
    () => getProject(slug!),
    pollingFetchConfig,
  );

  if (!slug) {
    return null;
  }

  const row = overview?.projects.find((project) => project.slug === slug);
  const summary = row ? cardSummary(row) : null;
  const items = detail ? viewOpenItems(detail) : [];
  const moving = viewMovingItems(items);
  const decisions = detail ? viewPendingDecisions(detail) : [];
  const mode = row ? parseMode(row) : null;
  const title = row?.title || slug;

  return (
    <aside className="hidden h-full w-72 shrink-0 flex-col border-l border-white/5 bg-white/[0.015] lg:flex">
      <div className="flex items-center gap-2 border-b border-white/5 px-3 py-2.5">
        <Link
          href="/"
          className="min-w-0 flex-1 truncate text-sm font-medium text-white/85 no-underline hover:text-white"
        >
          {title}
        </Link>
        {mode && (
          <span className="shrink-0 text-[10px] uppercase tracking-wider text-white/40">
            {mode.base}
          </span>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        {(summary?.nextAction || summary?.controllerBehind) && (
          <div className="mb-3 rounded-lg border border-white/[0.07] bg-white/[0.02] px-2.5 py-2">
            <div className="flex flex-wrap items-center gap-2 text-[11px] text-white/40">
              {summary.controllerBehind && (
                <span className="text-amber-200/80">controller behind</span>
              )}
              {summary.lastSignalAt && <span className="ml-auto">last signal</span>}
            </div>
            {summary.nextAction && (
              <p className="mt-1 text-xs text-white/75">
                <span className="text-white/40">Next: </span>
                {summary.nextAction}
              </p>
            )}
          </div>
        )}
        {decisions.map((decision) => (
          <p
            key={decision.question}
            className="mb-2 text-xs text-amber-100/90"
          >
            {decision.question}
          </p>
        ))}
        {moving.length > 0 && (
          <div className="mb-3">
            <p className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-white/35">
              Moving ({moving.length})
            </p>
            {moving.map((item) => (
              <div key={item.key} className="mb-1.5">
                <div className="truncate text-xs text-white/80">{item.key}</div>
                {item.attempts.slice(0, 1).map((attempt) => (
                  <Link
                    key={attempt.id}
                    href={`/control?mission=${attempt.id}`}
                    className={cn(
                      "mt-0.5 block truncate text-[11px] text-white/45 no-underline hover:text-white/70",
                    )}
                  >
                    {attempt.title || attempt.id.slice(0, 8)}
                  </Link>
                ))}
              </div>
            ))}
          </div>
        )}
        {items.length > moving.length && (
          <p className="text-[11px] text-white/35">
            {items.length - moving.length} stalled items
          </p>
        )}
      </div>
    </aside>
  );
}
