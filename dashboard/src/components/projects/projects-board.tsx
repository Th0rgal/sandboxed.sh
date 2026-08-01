"use client";

import Link from "next/link";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import useSWR from "swr";
import {
  AlertTriangle,
  Archive,
  ArrowLeft,
  GitPullRequest,
  Inbox,
  Loader,
  PauseCircle,
  Search,
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

const SECTIONS: { bucket: ProjectBucket; title: string; emoji: string }[] = [
  { bucket: "attention", title: "Attention requise", emoji: "🟠" },
  { bucket: "active", title: "Actif", emoji: "🟢" },
  { bucket: "paused", title: "Pausé", emoji: "⏸" },
  { bucket: "archived", title: "Archive", emoji: "📦" },
];

const LIVE_STATUSES = new Set([
  "created",
  "queued",
  "active",
  "pending",
  "waiting_background",
  "awaiting_user",
  "paused",
]);

function liveMissionCount(project: ProjectRow): number {
  return project.missions.filter((m) => LIVE_STATUSES.has(m.status)).length;
}

/** Latest signal timestamp for sorting: update, mission activity, or tracker mtime. */
function lastSignalMs(project: ProjectRow): number {
  const candidates = [
    project.latest_update?.at,
    project.missions[0]?.updated_at,
    project.tracker?.updated_at,
  ];
  return candidates.reduce((max, at) => {
    if (!at) return max;
    const ms = new Date(at).getTime();
    return Number.isNaN(ms) ? max : Math.max(max, ms);
  }, 0);
}

/** A project with no missions and no updates is a quiet tracker file. */
function isQuiet(project: ProjectRow): boolean {
  return project.missions.length === 0 && project.updates_count === 0;
}

export default function ProjectsBoard() {
  const { data, error, isLoading } = useSWR(
    "projects-overview",
    getProjectsOverview,
    { refreshInterval: 30000, revalidateOnFocus: false },
  );
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [mobileDetail, setMobileDetail] = useState(false);
  const listRef = useRef<HTMLDivElement | null>(null);

  const sections = useMemo(() => {
    const projects = data?.projects ?? [];
    const query = filter.trim().toLowerCase();
    const filtered = query
      ? projects.filter((p) => p.slug.toLowerCase().includes(query))
      : projects;
    return SECTIONS.map((section) => ({
      ...section,
      projects: filtered
        .filter((p) => p.bucket === section.bucket)
        .sort((a, b) => {
          const quiet = Number(isQuiet(a)) - Number(isQuiet(b));
          if (quiet !== 0) return quiet;
          return lastSignalMs(b) - lastSignalMs(a) || a.slug.localeCompare(b.slug);
        }),
    })).filter((section) => section.projects.length > 0);
  }, [data, filter]);

  const flatList = useMemo(
    () => sections.flatMap((section) => section.projects),
    [sections],
  );

  const selected =
    flatList.find((p) => p.slug === selectedSlug) ?? flatList[0] ?? null;

  const select = useCallback((slug: string, viaPointer = false) => {
    setSelectedSlug(slug);
    if (viaPointer) setMobileDetail(true);
  }, []);

  // Keyboard triage: ↑/↓ move the selection through the flattened list.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const target = event.target as HTMLElement | null;
      if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) {
        return;
      }
      if (flatList.length === 0) return;
      event.preventDefault();
      const currentIndex = selected
        ? flatList.findIndex((p) => p.slug === selected.slug)
        : -1;
      const nextIndex =
        event.key === "ArrowDown"
          ? Math.min(currentIndex + 1, flatList.length - 1)
          : Math.max(currentIndex - 1, 0);
      const next = flatList[nextIndex];
      if (next) {
        setSelectedSlug(next.slug);
        listRef.current
          ?.querySelector(`[data-slug="${CSS.escape(next.slug)}"]`)
          ?.scrollIntoView({ block: "nearest" });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [flatList, selected]);

  const attentionCount =
    data?.projects.filter((p) => p.bucket === "attention").length ?? 0;
  const liveTotal =
    data?.projects.reduce((sum, p) => sum + liveMissionCount(p), 0) ?? 0;
  const unrouted = data?.unrouted_updates ?? [];

  return (
    <div className="mx-auto flex h-[calc(100vh-1px)] max-w-[1500px] flex-col px-3 pt-3 sm:px-5">
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2 pb-3">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight text-white/90">
            Projets
          </h1>
          <p className="text-[11px] text-white/35">
            <Link
              href="/assistant/chat"
              className="transition-colors hover:text-white/60"
            >
              Le chat vit sous /assistant/chat →
            </Link>
          </p>
        </div>

        <div className="ml-auto flex items-center gap-2.5">
          {data && (
            <>
              <StatPill
                tone={attentionCount > 0 ? "amber" : "neutral"}
                label="attention"
                value={attentionCount}
              />
              <StatPill tone="sky" label="missions live" value={liveTotal} />
              <span className="hidden items-center gap-2.5 text-[11px] text-white/35 sm:flex">
                <SourceDot ok={data.sources.trackers} label="trackers" />
                <SourceDot ok={data.sources.hermes_db} label="hermes" />
              </span>
            </>
          )}
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-white/30" />
            <input
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder="Filtrer…"
              className="w-36 rounded-lg border border-white/[0.08] bg-white/[0.03] py-1.5 pl-8 pr-3 text-xs text-white/80 placeholder:text-white/25 focus:border-indigo-400/40 focus:outline-none sm:w-48"
            />
          </div>
        </div>
      </header>

      {error && (
        <div className="mb-3 rounded-lg border border-red-400/20 bg-red-500/10 px-3 py-2 text-sm text-red-300/90">
          Impossible de charger l’aperçu des projets : {String(error)}
        </div>
      )}
      {isLoading && !data && (
        <div className="flex items-center gap-2 py-10 text-sm text-white/40">
          <Loader className="h-4 w-4 animate-spin" /> Chargement des projets…
        </div>
      )}

      {data && (
        <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 pb-4 lg:grid-cols-[340px_minmax(0,1fr)] xl:grid-cols-[380px_minmax(0,1fr)]">
          {/* ── Triage list ── */}
          <div
            ref={listRef}
            className={cn(
              "min-h-0 overflow-y-auto rounded-xl border border-white/[0.06] bg-white/[0.015]",
              mobileDetail && "hidden lg:block",
            )}
          >
            {sections.map((section) => (
              <div key={section.bucket}>
                <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-white/[0.05] bg-[rgb(var(--background))]/95 px-3 py-1.5 backdrop-blur">
                  <span className="text-[11px] font-semibold uppercase tracking-wider text-white/45">
                    {section.emoji} {section.title}
                  </span>
                  <span className="text-[11px] text-white/25">
                    {section.projects.length}
                  </span>
                </div>
                {section.projects.map((project) => (
                  <ProjectListRow
                    key={project.slug}
                    project={project}
                    selected={selected?.slug === project.slug}
                    onSelect={() => select(project.slug, true)}
                  />
                ))}
              </div>
            ))}
            {flatList.length === 0 && (
              <p className="px-4 py-8 text-center text-sm text-white/30">
                Aucun projet ne correspond.
              </p>
            )}
            {unrouted.length > 0 && (
              <div className="border-t border-white/[0.05] px-3 py-2.5">
                <p className="mb-1 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-white/35">
                  <Inbox className="h-3 w-3" /> Non routées ({unrouted.length})
                </p>
                {unrouted.slice(0, 4).map((update, index) => (
                  <div
                    key={`${update.session_id}-${update.at}-${index}`}
                    className="flex min-w-0 items-center gap-2 py-0.5 text-[11px] text-white/40"
                  >
                    <span className="min-w-0 flex-1 truncate">
                      {update.headline || "(sans titre)"}
                    </span>
                    <UpdateAge at={update.at} />
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* ── Detail pane ── */}
          <div
            className={cn(
              "min-h-0 overflow-y-auto rounded-xl border border-white/[0.06] bg-white/[0.015]",
              !mobileDetail && "hidden lg:block",
            )}
          >
            {selected ? (
              <ProjectDetail
                project={selected}
                onBack={() => setMobileDetail(false)}
              />
            ) : (
              <p className="px-4 py-10 text-center text-sm text-white/30">
                Sélectionne un projet pour voir sa timeline.
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function StatPill({
  tone,
  label,
  value,
}: {
  tone: "amber" | "sky" | "neutral";
  label: string;
  value: number;
}) {
  return (
    <span
      className={cn(
        "flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px]",
        tone === "amber" && "border-amber-400/25 bg-amber-500/10 text-amber-200/90",
        tone === "sky" && "border-sky-400/25 bg-sky-500/[0.07] text-sky-300",
        tone === "neutral" && "border-white/[0.08] bg-white/[0.03] text-white/50",
      )}
    >
      <span className="font-semibold tabular-nums">{value}</span> {label}
    </span>
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

function railColor(project: ProjectRow): string {
  if (project.bucket === "attention") return "bg-amber-400/80";
  if (project.bucket === "paused") return "bg-white/20";
  if (project.bucket === "archived") return "bg-white/10";
  if (liveMissionCount(project) > 0) return "bg-emerald-400/80";
  return "bg-emerald-400/30";
}

function ProjectListRow({
  project,
  selected,
  onSelect,
}: {
  project: ProjectRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const live = liveMissionCount(project);
  const quiet = isQuiet(project);
  return (
    <button
      type="button"
      data-slug={project.slug}
      onClick={onSelect}
      className={cn(
        "group relative flex w-full items-stretch gap-2.5 px-3 py-2 text-left transition-colors",
        selected
          ? "bg-indigo-500/[0.09]"
          : "hover:bg-white/[0.03]",
      )}
    >
      <span
        className={cn(
          "w-[3px] shrink-0 self-stretch rounded-full",
          selected ? "bg-indigo-400" : railColor(project),
        )}
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline justify-between gap-2">
          <span
            className={cn(
              "truncate text-[13px] font-medium",
              quiet ? "text-white/45" : "text-white/85",
            )}
          >
            {project.slug}
          </span>
          {project.latest_update && (
            <UpdateAge at={project.latest_update.at} />
          )}
        </span>
        {!quiet && (
          <span className="mt-0.5 flex min-w-0 items-center gap-2 text-[11px] text-white/40">
            {live > 0 && (
              <span className="flex shrink-0 items-center gap-1 text-sky-300">
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-400" />
                {live} live
              </span>
            )}
            <span className="min-w-0 truncate">
              {project.attention_reasons[0] ??
                project.latest_update?.headline ??
                project.tracker?.status_line ??
                ""}
            </span>
          </span>
        )}
      </span>
      {project.bucket === "attention" && (
        <AlertTriangle className="mt-1 h-3.5 w-3.5 shrink-0 text-amber-300/80" />
      )}
      {project.bucket === "paused" && (
        <PauseCircle className="mt-1 h-3.5 w-3.5 shrink-0 text-white/25" />
      )}
      {project.bucket === "archived" && (
        <Archive className="mt-1 h-3.5 w-3.5 shrink-0 text-white/20" />
      )}
    </button>
  );
}

function bucketPill(bucket: ProjectBucket) {
  switch (bucket) {
    case "attention":
      return (
        <span className="rounded-full border border-amber-400/30 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-200/90">
          🟠 attention requise
        </span>
      );
    case "paused":
      return (
        <span className="rounded-full border border-white/[0.1] bg-white/[0.04] px-2 py-0.5 text-[10px] font-medium text-white/50">
          ⏸ pausé
        </span>
      );
    case "archived":
      return (
        <span className="rounded-full border border-white/[0.08] bg-white/[0.03] px-2 py-0.5 text-[10px] font-medium text-white/40">
          📦 archivé
        </span>
      );
    default:
      return (
        <span className="rounded-full border border-emerald-400/25 bg-emerald-500/[0.08] px-2 py-0.5 text-[10px] font-medium text-emerald-200/80">
          🟢 actif
        </span>
      );
  }
}

function ProjectDetail({
  project,
  onBack,
}: {
  project: ProjectRow;
  onBack: () => void;
}) {
  const { data, error, isLoading } = useSWR(
    ["project-updates", project.slug],
    () => getProjectUpdates(project.slug, 50),
    { revalidateOnFocus: false },
  );
  const updates = data?.updates ?? [];

  return (
    <div className="flex min-h-full flex-col">
      <div className="border-b border-white/[0.06] px-4 py-3 sm:px-5">
        <div className="flex items-center gap-2.5">
          <button
            type="button"
            onClick={onBack}
            className="rounded-md p-1 text-white/40 transition-colors hover:bg-white/[0.06] hover:text-white/80 lg:hidden"
            aria-label="Retour à la liste"
          >
            <ArrowLeft className="h-4 w-4" />
          </button>
          <h2 className="min-w-0 truncate text-base font-semibold text-white/90">
            {project.slug}
          </h2>
          {bucketPill(project.bucket)}
          {project.tracker?.updated_at && (
            <span className="ml-auto hidden shrink-0 text-[11px] text-white/30 sm:block">
              tracker <UpdateAge at={project.tracker.updated_at} />
            </span>
          )}
        </div>
        {project.tracker?.status_line && (
          <p className="mt-1.5 text-xs leading-relaxed text-white/55">
            {project.tracker.status_line}
          </p>
        )}
        {project.attention_reasons.length > 0 && (
          <div className="mt-2 space-y-1 rounded-lg border border-amber-400/20 bg-amber-500/[0.06] px-3 py-2">
            {project.attention_reasons.map((reason) => (
              <p
                key={reason}
                className="flex items-start gap-1.5 text-xs text-amber-200/90"
              >
                <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
                {reason}
              </p>
            ))}
          </div>
        )}
        {project.missions.length > 0 && (
          <div className="mt-2.5 flex flex-wrap gap-1.5">
            {project.missions.map((mission) => (
              <MissionMiniChip key={mission.id} mission={mission} />
            ))}
          </div>
        )}
      </div>

      <div className="flex-1 px-4 py-3 sm:px-5">
        <p className="mb-3 text-[11px] font-semibold uppercase tracking-wider text-white/35">
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
        <div className="relative space-y-2.5 before:absolute before:bottom-2 before:left-[5px] before:top-2 before:w-px before:bg-white/[0.07]">
          {updates.map((update, index) => (
            <UpdateEntry
              key={`${update.session_id}-${update.at}-${index}`}
              update={update}
              defaultExpanded={index === 0}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

const LIVE_DOT: Record<string, string> = {
  active: "bg-sky-400",
  queued: "bg-sky-400",
  created: "bg-sky-400",
  pending: "bg-amber-300",
  awaiting_user: "bg-amber-300",
  waiting_background: "bg-amber-300",
  paused: "bg-amber-300",
  completed: "bg-emerald-400",
  acknowledged: "bg-emerald-400",
  failed: "bg-red-400",
  interrupted: "bg-red-400",
  blocked: "bg-red-400",
  not_feasible: "bg-red-400",
};

function MissionMiniChip({ mission }: { mission: ProjectMissionChip }) {
  const live = LIVE_STATUSES.has(mission.status);
  return (
    <Link
      href={`/control?mission=${mission.id}`}
      onClick={(event) => event.stopPropagation()}
      title={`${mission.title ?? mission.id} — ${mission.status}`}
      className={cn(
        "inline-flex max-w-[200px] items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px] !no-underline transition-colors",
        live
          ? "border-sky-400/25 bg-sky-500/10 text-sky-300 hover:border-sky-400/50"
          : "border-white/[0.1] bg-white/[0.04] text-white/50 hover:border-white/25",
      )}
    >
      <span
        className={cn(
          "h-1.5 w-1.5 shrink-0 rounded-full",
          LIVE_DOT[mission.status] ?? "bg-white/30",
        )}
      />
      <span className="truncate">
        {mission.title || mission.id.slice(0, 8)}
      </span>
      {mission.github_pr && (
        <GitPullRequest className="h-3 w-3 shrink-0 opacity-70" />
      )}
    </Link>
  );
}

function UpdateAge({ at }: { at: string }) {
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return null;
  return (
    <RelativeTime
      date={date}
      className="shrink-0 text-[10px] tabular-nums text-white/30"
    />
  );
}

/** Body without the `[Cron delivery:…]` tag line and the headline it repeats. */
function bodyWithoutHeadline(body: string, headline: string): string {
  const lines = body.split("\n");
  let index = 0;
  while (index < lines.length) {
    const trimmed = lines[index].trim();
    if (
      trimmed.length === 0 ||
      trimmed.startsWith("[Cron delivery:") ||
      trimmed.replace(/^#+\s*/, "") === headline
    ) {
      index += 1;
      continue;
    }
    break;
  }
  return lines
    .slice(index)
    .filter((line) => line.trim() !== "[SILENT]")
    .join("\n");
}

function UpdateEntry({
  update,
  defaultExpanded,
}: {
  update: ProjectDeliveryUpdate;
  defaultExpanded: boolean;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  return (
    <div className="relative pl-6">
      <span
        className={cn(
          "absolute left-0 top-[13px] h-[11px] w-[11px] rounded-full border-2 border-[rgb(var(--background))]",
          update.blocker ? "bg-amber-400" : "bg-white/25",
        )}
      />
      <div
        className={cn(
          "rounded-lg border px-3 py-2 transition-colors",
          update.blocker
            ? "border-amber-400/20 bg-amber-500/[0.04]"
            : "border-white/[0.06] bg-white/[0.02]",
        )}
      >
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="flex w-full items-center justify-between gap-2 text-left"
        >
          <span className="min-w-0 flex-1 truncate text-[13px] text-white/80">
            {update.headline || "(update sans titre)"}
          </span>
          <UpdateAge at={update.at} />
        </button>
        {update.blocker && (
          <p className="mt-1 flex items-start gap-1.5 text-xs text-amber-300/90">
            <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
            Bloqué par : {update.blocker}
          </p>
        )}
        {expanded && update.body && (
          <div className="mt-2 border-t border-white/[0.05] pt-2 text-sm">
            <MarkdownContent
              content={bodyWithoutHeadline(update.body, update.headline)}
            />
            <p className="mt-2 text-[11px] text-white/30">
              Session d’origine : {update.session_id}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
