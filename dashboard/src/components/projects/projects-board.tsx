"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import useSWR from "swr";
import {
  AlertTriangle,
  Archive,
  CircleDot,
  ExternalLink,
  GitPullRequest,
  Inbox,
  Loader,
  PauseCircle,
  X,
} from "lucide-react";

import {
  getProjectsOverview,
  getProjectUpdates,
  type ProjectBucket,
  type ProjectDeliveryUpdate,
  type ProjectMissionChip,
  type ProjectRow,
} from "@/lib/api/projects";
import { MarkdownContent } from "@/components/markdown-content";
import { RelativeTime } from "@/components/ui/relative-time";
import { cn } from "@/lib/utils";

const COLUMNS: {
  bucket: ProjectBucket;
  title: string;
  emoji: string;
  empty: string;
}[] = [
  { bucket: "active", title: "Actif", emoji: "🟢", empty: "Aucun projet actif" },
  {
    bucket: "attention",
    title: "Attention requise",
    emoji: "🟠",
    empty: "Rien à signaler",
  },
  { bucket: "paused", title: "Pausé", emoji: "⏸", empty: "Aucun projet en pause" },
  { bucket: "archived", title: "Archive", emoji: "📦", empty: "Archive vide" },
];

export default function ProjectsBoard() {
  const { data, error, isLoading } = useSWR(
    "projects-overview",
    getProjectsOverview,
    { refreshInterval: 30000, revalidateOnFocus: false },
  );
  const [openSlug, setOpenSlug] = useState<string | null>(null);

  useEffect(() => {
    if (!openSlug) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenSlug(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [openSlug]);

  const close = useCallback(() => setOpenSlug(null), []);

  const projects = data?.projects ?? [];
  const unrouted = data?.unrouted_updates ?? [];
  const openProject = projects.find((p) => p.slug === openSlug) ?? null;

  return (
    <div className="mx-auto flex h-full max-w-[1600px] flex-col gap-4 px-4 py-4 sm:px-6">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-semibold text-white/90">Projets</h1>
          <p className="text-xs text-white/40">
            Trackers Hermes · missions taguées · updates cron.{" "}
            <Link href="/assistant/chat" className="text-indigo-300/80 hover:text-indigo-200">
              Le chat vit sous /assistant/chat
            </Link>
          </p>
        </div>
        {data && (
          <div className="flex items-center gap-3 text-[11px] text-white/40">
            <SourceDot ok={data.sources.trackers} label="trackers" />
            <SourceDot ok={data.sources.hermes_db} label="hermes db" />
          </div>
        )}
      </header>

      {error && (
        <div className="rounded-lg border border-red-400/20 bg-red-500/10 px-3 py-2 text-sm text-red-300/90">
          Impossible de charger l’aperçu des projets : {String(error)}
        </div>
      )}
      {isLoading && !data && (
        <div className="flex items-center gap-2 py-10 text-sm text-white/40">
          <Loader className="h-4 w-4 animate-spin" /> Chargement du board…
        </div>
      )}

      {data && (
        <div className="grid min-h-0 flex-1 grid-cols-1 gap-3 overflow-y-auto sm:grid-cols-2 xl:grid-cols-4">
          {COLUMNS.map((column) => {
            const rows = projects.filter((p) => p.bucket === column.bucket);
            return (
              <section
                key={column.bucket}
                className="flex min-h-[120px] flex-col rounded-xl border border-white/[0.06] bg-white/[0.02]"
              >
                <div className="flex items-center justify-between border-b border-white/[0.05] px-3 py-2">
                  <span className="text-sm font-medium text-white/75">
                    {column.emoji} {column.title}
                  </span>
                  <span className="rounded-full bg-white/[0.06] px-2 py-0.5 text-[11px] text-white/45">
                    {rows.length}
                  </span>
                </div>
                <div className="flex flex-col gap-2 overflow-y-auto p-2">
                  {rows.length === 0 ? (
                    <p className="px-1 py-3 text-center text-xs text-white/30">
                      {column.empty}
                    </p>
                  ) : (
                    rows.map((project) => (
                      <ProjectCard
                        key={project.slug}
                        project={project}
                        onOpen={() => setOpenSlug(project.slug)}
                      />
                    ))
                  )}
                </div>
              </section>
            );
          })}
        </div>
      )}

      {unrouted.length > 0 && (
        <section className="rounded-xl border border-white/[0.06] bg-white/[0.02] px-3 py-2">
          <div className="flex items-center gap-2 text-xs font-medium text-white/60">
            <Inbox className="h-3.5 w-3.5" /> Updates non routées ({unrouted.length})
          </div>
          <div className="mt-1 space-y-1">
            {unrouted.slice(0, 5).map((update, index) => (
              <div
                key={`${update.session_id}-${update.at}-${index}`}
                className="flex min-w-0 items-center gap-2 text-xs text-white/45"
              >
                <span className="min-w-0 flex-1 truncate">{update.headline || "(sans titre)"}</span>
                <UpdateAge at={update.at} />
              </div>
            ))}
          </div>
        </section>
      )}

      {openProject && <ProjectDrawer project={openProject} onClose={close} />}
    </div>
  );
}

function SourceDot({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span
        className={cn(
          "h-1.5 w-1.5 rounded-full",
          ok ? "bg-emerald-400" : "bg-amber-300",
        )}
      />
      {label}
    </span>
  );
}

function ProjectCard({
  project,
  onOpen,
}: {
  project: ProjectRow;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        "group w-full rounded-lg border px-3 py-2.5 text-left transition-colors",
        project.bucket === "attention"
          ? "border-amber-400/25 bg-amber-500/[0.06] hover:border-amber-400/40"
          : "border-white/[0.07] bg-white/[0.03] hover:border-indigo-400/30 hover:bg-white/[0.05]",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-sm font-medium text-white/85">
          {project.slug}
        </span>
        <BucketIcon bucket={project.bucket} />
      </div>

      {project.tracker?.status_line && (
        <p className="mt-1 line-clamp-2 text-xs text-white/50">
          {project.tracker.status_line}
        </p>
      )}

      {project.missions.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {project.missions.slice(0, 4).map((mission) => (
            <MissionMiniChip key={mission.id} mission={mission} />
          ))}
          {project.missions.length > 4 && (
            <span className="px-1 text-[10px] text-white/35">
              +{project.missions.length - 4}
            </span>
          )}
        </div>
      )}

      {project.latest_update && (
        <div className="mt-2 flex min-w-0 items-center gap-2 text-[11px] text-white/40">
          <span className="min-w-0 flex-1 truncate">
            {project.latest_update.headline || "(update sans titre)"}
          </span>
          <UpdateAge at={project.latest_update.at} />
        </div>
      )}

      {project.attention_reasons.length > 0 && (
        <div className="mt-2 flex items-start gap-1.5 text-[11px] text-amber-300/85">
          <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
          <span className="min-w-0">
            {project.attention_reasons.join(" · ")}
          </span>
        </div>
      )}
    </button>
  );
}

function BucketIcon({ bucket }: { bucket: ProjectBucket }) {
  if (bucket === "attention")
    return <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-amber-300/80" />;
  if (bucket === "paused")
    return <PauseCircle className="h-3.5 w-3.5 shrink-0 text-white/35" />;
  if (bucket === "archived")
    return <Archive className="h-3.5 w-3.5 shrink-0 text-white/30" />;
  return <CircleDot className="h-3.5 w-3.5 shrink-0 text-emerald-400/70" />;
}

const LIVE_STATUSES = new Set([
  "created",
  "queued",
  "active",
  "pending",
  "waiting_background",
  "awaiting_user",
  "paused",
]);

function MissionMiniChip({ mission }: { mission: ProjectMissionChip }) {
  const live = LIVE_STATUSES.has(mission.status);
  return (
    <Link
      href={`/control?mission=${mission.id}`}
      onClick={(event) => event.stopPropagation()}
      title={`${mission.title ?? mission.id} — ${mission.status}`}
      className={cn(
        "inline-flex max-w-[160px] items-center gap-1 rounded-full border px-1.5 py-0.5 text-[10px] !no-underline transition-colors",
        live
          ? "border-sky-400/25 bg-sky-500/10 text-sky-200/90 hover:border-sky-400/50"
          : "border-white/[0.1] bg-white/[0.04] text-white/50 hover:border-white/25",
      )}
    >
      <span
        className={cn(
          "h-1 w-1 shrink-0 rounded-full",
          missionDot(mission.status),
        )}
      />
      <span className="truncate">
        {mission.title || mission.id.slice(0, 8)}
      </span>
      {mission.github_pr && <GitPullRequest className="h-2.5 w-2.5 shrink-0" />}
    </Link>
  );
}

function missionDot(status: string) {
  switch (status) {
    case "active":
    case "queued":
    case "created":
      return "bg-sky-400";
    case "pending":
    case "awaiting_user":
    case "waiting_background":
    case "paused":
      return "bg-amber-300";
    case "completed":
    case "acknowledged":
      return "bg-emerald-400";
    case "failed":
    case "interrupted":
    case "blocked":
    case "not_feasible":
      return "bg-red-400";
    default:
      return "bg-white/30";
  }
}

function UpdateAge({ at }: { at: string }) {
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return null;
  return (
    <RelativeTime date={date} className="shrink-0 text-[10px] text-white/30" />
  );
}

function ProjectDrawer({
  project,
  onClose,
}: {
  project: ProjectRow;
  onClose: () => void;
}) {
  const { data, error, isLoading } = useSWR(
    ["project-updates", project.slug],
    () => getProjectUpdates(project.slug, 50),
    { revalidateOnFocus: false },
  );
  const updates = data?.updates ?? [];

  return (
    <div className="fixed inset-0 z-50 flex justify-end">
      <button
        type="button"
        aria-label="Fermer"
        onClick={onClose}
        className="absolute inset-0 bg-black/50 backdrop-blur-[2px]"
      />
      <aside className="relative flex h-full w-full max-w-2xl flex-col border-l border-white/[0.08] bg-[rgb(var(--background))] shadow-2xl">
        <div className="flex items-start justify-between gap-3 border-b border-white/[0.06] px-4 py-3">
          <div className="min-w-0">
            <h2 className="truncate text-base font-semibold text-white/90">
              {project.slug}
            </h2>
            {project.tracker?.status_line && (
              <p className="mt-0.5 text-xs text-white/50">
                {project.tracker.status_line}
              </p>
            )}
            {project.attention_reasons.length > 0 && (
              <div className="mt-1.5 flex items-start gap-1.5 text-xs text-amber-300/85">
                <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
                <span>{project.attention_reasons.join(" · ")}</span>
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md p-1.5 text-white/40 transition-colors hover:bg-white/[0.06] hover:text-white/80"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {project.missions.length > 0 && (
          <div className="border-b border-white/[0.06] px-4 py-2.5">
            <p className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-white/35">
              Missions
            </p>
            <div className="flex flex-wrap gap-1.5">
              {project.missions.map((mission) => (
                <MissionMiniChip key={mission.id} mission={mission} />
              ))}
            </div>
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          <p className="mb-2 text-[11px] font-medium uppercase tracking-wide text-white/35">
            Timeline des updates
          </p>
          {isLoading && (
            <div className="flex items-center gap-2 py-6 text-sm text-white/40">
              <Loader className="h-4 w-4 animate-spin" /> Chargement…
            </div>
          )}
          {error && (
            <p className="py-4 text-sm text-red-300/80">
              Impossible de charger les updates.
            </p>
          )}
          {!isLoading && !error && updates.length === 0 && (
            <p className="py-4 text-sm text-white/35">
              Aucune update routée vers ce projet.
            </p>
          )}
          <div className="space-y-3">
            {updates.map((update, index) => (
              <UpdateEntry
                key={`${update.session_id}-${update.at}-${index}`}
                update={update}
              />
            ))}
          </div>
        </div>
      </aside>
    </div>
  );
}

function UpdateEntry({ update }: { update: ProjectDeliveryUpdate }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5">
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="flex w-full items-center justify-between gap-2 text-left"
      >
        <span className="min-w-0 flex-1 truncate text-sm text-white/80">
          {update.headline || "(update sans titre)"}
        </span>
        <UpdateAge at={update.at} />
      </button>
      {update.blocker && (
        <p className="mt-1 flex items-start gap-1.5 text-xs text-amber-300/85">
          <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
          Bloqué par : {update.blocker}
        </p>
      )}
      {expanded && update.body && (
        <div className="mt-2 border-t border-white/[0.05] pt-2 text-sm">
          <MarkdownContent content={update.body} />
          <div className="mt-2 text-[11px] text-white/30">
            <span className="inline-flex items-center gap-1">
              <ExternalLink className="h-3 w-3" />
              Session d’origine : {update.session_id}
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
