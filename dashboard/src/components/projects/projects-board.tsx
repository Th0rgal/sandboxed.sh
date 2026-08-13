"use client";

import Link from "next/link";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import useSWR, { useSWRConfig } from "swr";
import type { LucideIcon } from "lucide-react";
import {
  Activity,
  AlertTriangle,
  Archive,
  ArchiveRestore,
  ArrowLeft,
  ChevronDown,
  ChevronRight,
  GitPullRequest,
  Inbox,
  Link2,
  Loader,
  Pause,
  Play,
  Search,
  Send,
  Sparkles,
  Trash2,
  Unlink,
} from "lucide-react";

import {
  bindProjectConversation,
  unbindProjectConversation,
  type ProjectConversation,
  getProjectsOverview,
  getProjectUpdates,
  postProjectAction,
  type ProjectAction,
  type ProjectBucket,
  type ProjectDeliveryUpdate,
  type ProjectMissionChip,
  type ProjectRow,
} from "@/lib/api/projects";
import {
  listHermesSessions,
  type HermesSession,
} from "@/lib/api/hermes";
import { hermesChatStream } from "@/lib/api/hermes";
import { MarkdownContent } from "@/components/markdown-content";
import { RelativeTime } from "@/components/ui/relative-time";
import { cn } from "@/lib/utils";
import {
  healthDigest,
  isStale,
  ModeChip,
  parseMode,
  TrackHealthList,
} from "./project-health";

// Palette discipline: neutrals for structure, indigo for the current
// selection, amber for problems. Nothing else carries color.
const SECTIONS: {
  bucket: ProjectBucket;
  title: string;
  icon: LucideIcon;
}[] = [
  { bucket: "attention", title: "Needs attention", icon: AlertTriangle },
  { bucket: "active", title: "Active", icon: Activity },
  { bucket: "paused", title: "Paused", icon: Pause },
  { bucket: "archived", title: "Archive", icon: Archive },
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

const PROBLEM_STATUSES = new Set([
  "failed",
  "interrupted",
  "blocked",
  "not_feasible",
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

/** Flatten markdown syntax for one-line headlines (bold, code, links, headings). */
function stripMarkdown(text: string): string {
  return text
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/(\*\*|__)(.*?)\1/g, "$2")
    .replace(/(\*|_)(.*?)\1/g, "$2")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/^#+\s*/, "")
    .trim();
}

// ── Unread tracking ─────────────────────────────────────────────────────────
// Client-side only: the backend has no per-user read state, so "seen" lives in
// localStorage keyed by project slug. Opening a project in the detail pane
// records its current updates_count / latest_update.at; the badge is the delta.

const LAST_SEEN_STORAGE_KEY = "projects-board.last-seen.v1";

export type ProjectLastSeen = {
  updates_count: number;
  latest_at: string | null;
};

function loadLastSeen(): Record<string, ProjectLastSeen> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(LAST_SEEN_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return {};
    const result: Record<string, ProjectLastSeen> = {};
    for (const [slug, value] of Object.entries(
      parsed as Record<string, unknown>,
    )) {
      if (!value || typeof value !== "object") continue;
      const entry = value as Partial<ProjectLastSeen>;
      if (typeof entry.updates_count !== "number") continue;
      result[slug] = {
        updates_count: entry.updates_count,
        latest_at: typeof entry.latest_at === "string" ? entry.latest_at : null,
      };
    }
    return result;
  } catch {
    return {};
  }
}

function persistLastSeen(map: Record<string, ProjectLastSeen>) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(LAST_SEEN_STORAGE_KEY, JSON.stringify(map));
  } catch {
    // Storage full or unavailable — the badge just won't persist.
  }
}

/** New deliveries since the project was last opened. Exported for tests. */
export function unreadCountFor(
  project: Pick<ProjectRow, "updates_count" | "latest_update">,
  seen: ProjectLastSeen | undefined,
): number {
  if (!seen) return Math.max(0, project.updates_count);
  const delta = project.updates_count - seen.updates_count;
  if (delta > 0) return delta;
  // The rolling updates window can keep the count flat while newer deliveries
  // replace older ones — a fresher latest_update.at still means "something new".
  const latest = project.latest_update?.at ?? null;
  if (!latest) return 0;
  if (!seen.latest_at) return 1;
  const latestMs = new Date(latest).getTime();
  const seenMs = new Date(seen.latest_at).getTime();
  if (Number.isNaN(latestMs) || Number.isNaN(seenMs)) return 0;
  return latestMs > seenMs ? 1 : 0;
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

  // Loaded in an effect (not the initial state) so the server render and the
  // first client render agree — localStorage only exists on the client.
  const [lastSeen, setLastSeen] = useState<Record<string, ProjectLastSeen>>({});
  useEffect(() => {
    setLastSeen(loadLastSeen());
  }, []);

  const sections = useMemo(() => {
    const projects = data?.projects ?? [];
    const query = filter.trim().toLowerCase();
    const filtered = query
      ? projects.filter(
          (p) =>
            p.slug.toLowerCase().includes(query) ||
            (p.title ?? "").toLowerCase().includes(query),
        )
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

  // Opening a project (it is showing in the detail pane) marks it seen.
  const selectedUpdatesCount = selected?.updates_count;
  const selectedLatestAt = selected?.latest_update?.at ?? null;
  useEffect(() => {
    if (!selected) return;
    setLastSeen((prev) => {
      const entry = prev[selected.slug];
      if (
        entry &&
        entry.updates_count === selected.updates_count &&
        entry.latest_at === (selected.latest_update?.at ?? null)
      ) {
        return prev;
      }
      const next = {
        ...prev,
        [selected.slug]: {
          updates_count: selected.updates_count,
          latest_at: selected.latest_update?.at ?? null,
        },
      };
      persistLastSeen(next);
      return next;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.slug, selectedUpdatesCount, selectedLatestAt]);

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
  const degradedSources = data
    ? [
        !data.sources.trackers && "trackers",
        !data.sources.hermes_db && "hermes",
      ].filter(Boolean)
    : [];

  return (
    <div className="mx-auto flex h-[calc(100vh-1px)] max-w-[1500px] flex-col px-3 pt-3 sm:px-5">
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2 pb-3">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight text-white/90">
            Projects
          </h1>
          <p className="text-[11px] text-white/35">
            <Link
              href="/assistant/chat"
              className="transition-colors hover:text-white/60"
            >
              Chat lives at /assistant/chat →
            </Link>
          </p>
        </div>

        <div className="ml-auto flex items-center gap-2.5">
          {data && (
            <span className="flex items-center gap-3 text-[11px]">
              {attentionCount > 0 && (
                <span className="flex items-center gap-1 text-white/40">
                  <AlertTriangle className="h-3 w-3" />
                  {attentionCount} need attention
                </span>
              )}
              <span className="text-white/40">{liveTotal} live</span>
              {degradedSources.length > 0 && (
                <span className="hidden items-center gap-1 text-amber-300 sm:flex">
                  <AlertTriangle className="h-3 w-3" />
                  {degradedSources.join(" + ")} source unavailable
                </span>
              )}
            </span>
          )}
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-white/30" />
            <input
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder="Filter…"
              className="w-36 rounded-lg border border-white/[0.08] bg-white/[0.03] py-1.5 pl-8 pr-3 text-xs text-white/80 placeholder:text-white/25 focus:border-indigo-400/40 focus:outline-none sm:w-48"
            />
          </div>
        </div>
      </header>

      {error && (
        <div className="mb-3 flex items-center gap-2 rounded-lg border border-amber-400/20 bg-amber-500/[0.06] px-3 py-2 text-sm text-amber-200/90">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          Failed to load the projects overview: {String(error)}
        </div>
      )}
      {isLoading && !data && <BoardSkeleton />}

      {data && (
        <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 pb-4 lg:grid-cols-[340px_minmax(0,1fr)] xl:grid-cols-[380px_minmax(0,1fr)]">
          {/* ── Triage list ── */}
          <div
            className={cn(
              "min-h-0 overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02]",
              mobileDetail && "hidden lg:block",
            )}
          >
            <div ref={listRef} className="h-full overflow-y-auto py-1">
            <SummaryStrip projects={data?.projects ?? []} />
            {sections.map((section) => (
              <div key={section.bucket}>
                <div className="sticky -top-1 z-10 flex items-center gap-2 border-b border-white/[0.05] bg-[rgb(var(--background))]/95 px-4 py-1.5 backdrop-blur">
                  <section.icon
                    className={cn(
                      "h-3 w-3",
                      section.bucket === "attention"
                        ? "text-amber-300/90"
                        : "text-white/30",
                    )}
                  />
                  <span className="text-[11px] font-semibold uppercase tracking-wider text-white/45">
                    {section.title}
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
                    unread={
                      selected?.slug === project.slug
                        ? 0
                        : unreadCountFor(project, lastSeen[project.slug])
                    }
                    onSelect={() => select(project.slug, true)}
                  />
                ))}
              </div>
            ))}
            {flatList.length === 0 && (
              <p className="px-4 py-8 text-center text-sm text-white/30">
                No matching project.
              </p>
            )}
            {unrouted.length > 0 && (
              <div className="border-t border-white/[0.05] px-4 py-2.5">
                <p className="mb-1 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-white/35">
                  <Inbox className="h-3 w-3" /> Unrouted ({unrouted.length})
                </p>
                {unrouted.slice(0, 4).map((update, index) => (
                  <div
                    key={`${update.session_id}-${update.at}-${index}`}
                    className="flex min-w-0 items-center gap-2 py-0.5 text-[11px] text-white/40"
                  >
                    <span className="min-w-0 flex-1 truncate">
                      {stripMarkdown(update.headline) || "(untitled)"}
                    </span>
                    <UpdateAge at={update.at} />
                  </div>
                ))}
              </div>
            )}
            </div>
          </div>

          {/* ── Detail pane ── */}
          <div
            className={cn(
              "min-h-0 overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02]",
              !mobileDetail && "hidden lg:block",
            )}
          >
            <div className="h-full overflow-y-auto">
              {selected ? (
                <ProjectDetail
                  project={selected}
                  onBack={() => setMobileDetail(false)}
                />
              ) : (
                <p className="px-4 py-10 text-center text-sm text-white/30">
                  Select a project to see its timeline.
                </p>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** One-line fleet recap above the list: the "how is everything doing" answer
 *  that previously required opening each project in turn. Counts blocked and
 *  silent separately because they are different failures — one reported
 *  itself, the other stopped reporting at all. */
function SummaryStrip({ projects }: { projects: ProjectRow[] }) {
  const counts = useMemo(() => {
    let blocked = 0;
    let paused = 0;
    let silent = 0;
    let attention = 0;
    let live = 0;
    for (const project of projects) {
      if (project.bucket === "archived") continue;
      const mode = parseMode(project);
      if (mode?.base === "blocked") blocked += 1;
      if (mode?.base === "paused" || project.bucket === "paused") paused += 1;
      if (isStale(project)) silent += 1;
      if (project.bucket === "attention") attention += 1;
      live += liveMissionCount(project);
    }
    return { blocked, paused, silent, attention, live };
  }, [projects]);

  const parts = [
    counts.live > 0 ? `${counts.live} live` : null,
    counts.attention > 0 ? `${counts.attention} need attention` : null,
    counts.blocked > 0 ? `${counts.blocked} blocked` : null,
    counts.silent > 0 ? `${counts.silent} silent` : null,
    counts.paused > 0 ? `${counts.paused} paused` : null,
  ].filter(Boolean) as string[];

  if (parts.length === 0) return null;
  return (
    <p className="px-4 py-1.5 text-[11px] text-white/40">{parts.join(" · ")}</p>
  );
}

function ProjectListRow({
  project,
  selected,
  unread,
  onSelect,
}: {
  project: ProjectRow;
  selected: boolean;
  /** New deliveries since this project was last opened; 0 hides the badge. */
  unread: number;
  onSelect: () => void;
}) {
  const live = liveMissionCount(project);
  const quiet = isQuiet(project);
  const mode = parseMode(project);
  const digest = healthDigest(project.health);
  const stale = isStale(project);
  return (
    <button
      type="button"
      data-slug={project.slug}
      onClick={onSelect}
      className={cn(
        "group relative mx-1.5 flex w-[calc(100%-0.75rem)] items-stretch gap-2.5 rounded-md px-2.5 py-2 text-left transition-colors",
        selected ? "bg-indigo-500/[0.09]" : "hover:bg-white/[0.03]",
      )}
    >
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline justify-between gap-2">
          <span
            className={cn(
              "truncate text-[13px] font-medium",
              quiet ? "text-white/45" : "text-white/85",
            )}
          >
            {project.title || project.slug}
          </span>
          <span className="flex shrink-0 items-center gap-1.5">
            {unread > 0 && (
              <span
                title={`${unread > 9 ? "9+" : unread} new update${unread === 1 ? "" : "s"}`}
                className="rounded-full bg-indigo-500/20 px-1.5 py-px text-[10px] font-semibold tabular-nums text-indigo-200"
              >
                {unread > 9 ? "9+" : unread}
              </span>
            )}
            {project.latest_update && (
              <UpdateAge at={project.latest_update.at} />
            )}
          </span>
        </span>
        {/* A blocked or stale controller is worth a second line even when the
            project is otherwise quiet — silence was exactly how a stuck
            controller used to hide. */}
        {(!quiet || mode?.base === "blocked" || stale) && (
          <span className="mt-0.5 flex min-w-0 items-center gap-2 text-[11px] text-white/40">
            {live > 0 && (
              <span className="flex shrink-0 items-center gap-1 text-white/55">
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[rgb(var(--text)/0.6)]" />
                {live}
              </span>
            )}
            <ModeChip mode={mode} />
            {project.delivery_health === "dropped" ||
            project.delivery_health === "misrouted" ? (
              <span
                title={project.delivery_health}
                className="shrink-0 text-[10px] uppercase tracking-wide text-amber-400/70"
              >
                {project.delivery_health === "dropped" ? "undelivered" : "misrouted"}
              </span>
            ) : null}
            {stale && (
              <span className="shrink-0 text-[10px] uppercase tracking-wide text-amber-400/70">
                silent
              </span>
            )}
            <span className="min-w-0 truncate">
              {digest ??
                stripMarkdown(
                  project.attention_reasons[0] ??
                    project.latest_update?.headline ??
                    project.tracker?.status_line ??
                    "",
                )}
            </span>
          </span>
        )}
      </span>
    </button>
  );
}

/** Icon button for the detail-pane action bar. */
/** One control in the project action row. Renders a link when `href` is given
 * (navigation, so middle-click and open-in-new-tab work), a button otherwise. */
function ActionButton({
  icon: Icon,
  label,
  title,
  onClick,
  href,
  busy,
  danger,
}: {
  icon: LucideIcon;
  label: string;
  /** Hover text when the label alone does not explain the consequence. */
  title?: string;
  onClick?: () => void;
  href?: string;
  busy?: boolean;
  danger?: boolean;
}) {
  const className = cn(
    "flex items-center gap-1.5 rounded-lg border px-2 py-1 text-[11px] transition-colors disabled:opacity-50",
    danger
      ? "border-amber-400/25 bg-amber-500/10 text-amber-300 hover:border-amber-400/50"
      : "border-white/[0.08] bg-white/[0.03] text-white/50 hover:border-white/20 hover:text-white/80",
  );
  const content = (
    <>
      {busy ? (
        <Loader className="h-3 w-3 animate-spin" />
      ) : (
        <Icon className="h-3 w-3" />
      )}
      {label}
    </>
  );

  if (href) {
    return (
      <Link href={href} title={title ?? label} className={cn(className, "no-underline")}>
        {content}
      </Link>
    );
  }

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      title={title ?? label}
      className={className}
    >
      {content}
    </button>
  );
}

function ProjectActions({ project }: { project: ProjectRow }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<ProjectAction | "bind" | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const run = useCallback(
    async (action: ProjectAction) => {
      setBusy(action);
      setActionError(null);
      try {
        await postProjectAction(project.slug, action);
        await mutate("projects-overview");
      } catch (err) {
        setActionError(String(err instanceof Error ? err.message : err));
      } finally {
        setBusy(null);
        setConfirmDelete(false);
      }
    },
    [project.slug, mutate],
  );

  // The conversation this project reports into. A declared binding is
  // authoritative; otherwise the newest delivery's session is only a GUESS —
  // and for a cron-driven project that guess is a throwaway per-tick session
  // that has already ended, which is why binding it is offered right here.
  // The dashboard ships via Vercel and the backend deploys separately, so a
  // window exists where this field is not served yet. Fall back to the old
  // inference rather than dropping the button — tagged as a guess either way.
  const conversation: ProjectConversation | null =
    project.conversation ??
    (project.latest_update?.session_id
      ? {
          session_id: project.latest_update.session_id,
          source: "latest_update" as const,
        }
      : null);
  const conversationSessionId = conversation?.session_id ?? null;
  const conversationIsGuess = conversation?.source === "latest_update";

  const [picking, setPicking] = useState(false);
  const [sessions, setSessions] = useState<HermesSession[] | null>(null);

  // Offer only conversations that can actually receive something. An ended
  // session is unreachable whatever produced it, and a per-tick cron session
  // is unreachable by construction — binding either would cement exactly the
  // corpse this feature exists to avoid. `ended_at` is the general rule; the
  // `cron_` check also catches a tick still in flight, which has no
  // `ended_at` yet but will never be a conversation.
  const openPicker = useCallback(async () => {
    setPicking(true);
    setActionError(null);
    if (sessions) return;
    try {
      const all = await listHermesSessions(50);
      setSessions(
        all.filter((s) => !s.ended_at && !s.id.startsWith("cron_")),
      );
    } catch (err) {
      setActionError(String(err instanceof Error ? err.message : err));
    }
  }, [sessions]);

  const bindConversation = useCallback(
    async (sessionId: string) => {
      setBusy("bind");
      setActionError(null);
      try {
        await bindProjectConversation(project.slug, sessionId);
        await mutate("projects-overview");
        setPicking(false);
      } catch (err) {
        setActionError(String(err instanceof Error ? err.message : err));
      } finally {
        setBusy(null);
      }
    },
    [project.slug, mutate],
  );

  const unbindConversation = useCallback(async () => {
    setBusy("bind");
    setActionError(null);
    try {
      await unbindProjectConversation(project.slug);
      await mutate("projects-overview");
    } catch (err) {
      setActionError(String(err instanceof Error ? err.message : err));
    } finally {
      setBusy(null);
    }
  }, [project.slug, mutate]);

  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {conversationSessionId && (
        <ActionButton
          icon={Sparkles}
          label="Conversation"
          href={`/control?session=${encodeURIComponent(conversationSessionId)}`}
        />
      )}
      {conversationIsGuess ? (
        <ActionButton
          icon={Link2}
          label="Bind…"
          title="Choose the conversation this project reports into. Until one is declared, the link above follows whichever conversation sent the last update — for a cron-driven project that is a per-tick conversation which has already ended."
          busy={busy === "bind"}
          onClick={openPicker}
        />
      ) : (
        conversation && (
          <ActionButton
            icon={Link2}
            label="Rebind…"
            title="Point this project at a different control conversation."
            busy={busy === "bind"}
            onClick={openPicker}
          />
        )
      )}
      {project.bucket === "paused" ? (
        <ActionButton
          icon={Play}
          label="Resume"
          busy={busy === "resume"}
          onClick={() => run("resume")}
        />
      ) : (
        <ActionButton
          icon={Pause}
          label="Pause"
          busy={busy === "pause"}
          onClick={() => run("pause")}
        />
      )}
      {project.bucket === "archived" ? (
        <ActionButton
          icon={ArchiveRestore}
          label="Unarchive"
          busy={busy === "unarchive"}
          onClick={() => run("unarchive")}
        />
      ) : (
        <ActionButton
          icon={Archive}
          label="Archive"
          busy={busy === "archive"}
          onClick={() => run("archive")}
        />
      )}
      <ActionButton
        icon={Trash2}
        label={confirmDelete ? "Confirm?" : "Delete"}
        danger={confirmDelete}
        busy={busy === "delete"}
        onClick={() => {
          if (confirmDelete) {
            void run("delete");
          } else {
            setConfirmDelete(true);
            window.setTimeout(() => setConfirmDelete(false), 4000);
          }
        }}
      />
      {conversation?.source === "binding" && (
        <ActionButton
          icon={Unlink}
          label="Unbind"
          title="Stop declaring a control conversation for this project. The link falls back to whichever conversation sent the last update."
          busy={busy === "bind"}
          onClick={unbindConversation}
        />
      )}
      {picking && (
        <div className="mt-1 flex w-full flex-wrap items-center gap-1.5">
          <select
            className="min-w-0 flex-1 rounded-md border border-white/[0.06] bg-white/[0.03] px-2 py-1 text-[11px] text-[rgb(var(--text))]"
            defaultValue=""
            onChange={(event) => {
              if (event.target.value) void bindConversation(event.target.value);
            }}
          >
            <option value="" disabled>
              {sessions === null
                ? "Loading conversations…"
                : sessions.length === 0
                  ? "No conversation available"
                  : "Choose the control conversation…"}
            </option>
            {(sessions ?? []).map((session) => (
              <option key={session.id} value={session.id}>
                {session.title || session.preview || session.id}
              </option>
            ))}
          </select>
          <ActionButton
            icon={ArrowLeft}
            label="Cancel"
            onClick={() => setPicking(false)}
          />
        </div>
      )}
      {actionError && (
        <span className="text-[11px] text-amber-300">{actionError}</span>
      )}
    </div>
  );
}

/** Inline reply into the Hermes session an update came from. */
function ReplyComposer({ sessionId }: { sessionId: string }) {
  const [message, setMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [reply, setReply] = useState<string | null>(null);
  const [replyDone, setReplyDone] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

  const send = useCallback(async () => {
    const text = message.trim();
    if (!text || sending) return;
    setSending(true);
    setSendError(null);
    setReply("");
    setReplyDone(false);
    try {
      let streamed = "";
      await hermesChatStream(sessionId, text, {
        onDelta: (delta) => {
          streamed += delta;
          setReply(streamed);
        },
        onCompleted: (content) => {
          setReply(content);
          setReplyDone(true);
        },
        onError: (errorMessage) => setSendError(errorMessage),
      });
      setMessage("");
    } catch (err) {
      setSendError(String(err instanceof Error ? err.message : err));
    } finally {
      setSending(false);
      setReplyDone(true);
    }
  }, [message, sending, sessionId]);

  return (
    <div className="mt-2 border-t border-white/[0.05] pt-2">
      <div className="flex items-end gap-2">
        <textarea
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
              event.preventDefault();
              void send();
            }
          }}
          placeholder={`Reply in conversation ${sessionId.slice(0, 14)}…`}
          rows={message.includes("\n") ? 3 : 1}
          className="min-h-[32px] flex-1 resize-none rounded-lg border border-white/[0.08] bg-white/[0.03] px-2.5 py-1.5 text-xs text-white/80 placeholder:text-white/25 focus:border-indigo-400/40 focus:outline-none"
        />
        <button
          type="button"
          onClick={() => void send()}
          disabled={sending || message.trim().length === 0}
          title="Send (⌘↵)"
          className="flex h-[32px] w-[32px] shrink-0 items-center justify-center rounded-lg border border-white/[0.08] bg-white/[0.03] text-white/50 transition-colors hover:border-indigo-400/40 hover:text-white/85 disabled:opacity-40"
        >
          {sending ? (
            <Loader className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Send className="h-3.5 w-3.5" />
          )}
        </button>
      </div>
      {sendError && (
        <p className="mt-1.5 flex items-center gap-1.5 text-[11px] text-amber-300">
          <AlertTriangle className="h-3 w-3 shrink-0" /> {sendError}
        </p>
      )}
      {reply !== null && !sendError && (
        <div className="mt-2 rounded-lg border border-indigo-400/15 bg-indigo-500/[0.04] px-3 py-2 text-sm">
          {reply === "" ? (
            <p className="flex items-center gap-2 text-xs text-white/40">
              <Loader className="h-3 w-3 animate-spin" /> Hermes is replying…
            </p>
          ) : (
            <MarkdownContent content={reply} />
          )}
          {replyDone && reply !== "" && (
            <p className="mt-1.5 text-[10px] text-white/30">
              Reply from conversation {sessionId.slice(0, 14)}
            </p>
          )}
        </div>
      )}
    </div>
  );
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
  const section = SECTIONS.find((s) => s.bucket === project.bucket);

  return (
    <div className="flex min-h-full flex-col">
      <div className="border-b border-white/[0.06] px-4 py-3 sm:px-5">
        <div className="flex items-center gap-2.5">
          <button
            type="button"
            onClick={onBack}
            className="rounded-md p-1 text-white/40 transition-colors hover:bg-white/[0.06] hover:text-white/80 lg:hidden"
            aria-label="Back to list"
          >
            <ArrowLeft className="h-4 w-4" />
          </button>
          <h2
            title={project.title ? project.slug : undefined}
            className="min-w-0 truncate text-base font-semibold text-white/90"
          >
            {project.title || project.slug}
          </h2>
          {section && (
            <span className="flex shrink-0 items-center gap-1.5 rounded-full border border-white/[0.08] bg-white/[0.03] px-2 py-0.5 text-[10px] font-medium text-white/45">
              <section.icon
                className={cn(
                  "h-3 w-3",
                  project.bucket === "attention" && "text-amber-300/90",
                )}
              />
              {section.title.toLowerCase()}
            </span>
          )}
          <span className="ml-auto">
            <ProjectActions project={project} />
          </span>
        </div>
        {project.tracker?.status_line && (
          <p className="mt-1.5 text-xs leading-relaxed text-white/55">
            {project.tracker.status_line}
          </p>
        )}
        {project.attention_reasons.length > 0 && (
          <div className="mt-2 space-y-1 rounded-lg border border-white/[0.07] bg-white/[0.02] px-3 py-2">
            {project.attention_reasons.map((reason) => (
              <p
                key={reason}
                className="flex items-start gap-1.5 text-xs text-white/60"
              >
                <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0 text-amber-300/80" />
                {reason}
              </p>
            ))}
          </div>
        )}
        {project.health && project.health.tracks.length > 0 && (
          <div className="mt-2 rounded-lg border border-white/[0.07] bg-white/[0.02] px-3 py-2">
            <p className="mb-1 text-[10px] uppercase tracking-wider text-white/40">
              Tracks
            </p>
            <TrackHealthList health={project.health} />
          </div>
        )}
        {project.missions.length > 0 && <MissionList missions={project.missions} />}
      </div>

      <div className="flex-1 px-4 py-3 sm:px-5">
        <p className="mb-3 text-[11px] font-semibold uppercase tracking-wider text-white/35">
          Updates
        </p>
        {isLoading && (
          <div className="flex items-center gap-2 py-6 text-sm text-white/40">
            <Loader className="h-4 w-4 animate-spin" /> Loading…
          </div>
        )}
        {error && (
          <p className="flex items-center gap-2 py-4 text-sm text-amber-200/90">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            Failed to load updates.
          </p>
        )}
        {!isLoading && !error && updates.length === 0 && (
          <p className="py-4 text-sm text-white/35">
            No updates routed to this project.
          </p>
        )}
        <div className="space-y-2.5">
          {updates.map((update, index) => (
            <UpdateEntry
              key={`${update.session_id}-${update.at}-${index}`}
              update={update}
              defaultExpanded={index === 0}
              replyToSessionId={
                project.conversation?.source === "binding"
                  ? project.conversation.session_id
                  : update.session_id
              }
            />
          ))}
        </div>
      </div>
    </div>
  );
}

/** Collapsible mission list for the detail header: summary row, expand for detail. */
function MissionList({ missions }: { missions: ProjectMissionChip[] }) {
  const [expanded, setExpanded] = useState(false);
  const live = missions.filter((m) => LIVE_STATUSES.has(m.status)).length;
  const problems = missions.filter((m) => PROBLEM_STATUSES.has(m.status)).length;
  return (
    <div className="mt-2.5">
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="flex w-full items-center gap-1.5 rounded-md py-1 text-left text-[11px] font-semibold uppercase tracking-wider text-white/35 transition-colors hover:text-white/60"
      >
        {expanded ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
        Missions ({missions.length})
        <span className="font-normal normal-case tracking-normal text-white/30">
          {live > 0 && ` · ${live} live`}
          {problems > 0 && ` · ${problems} failed`}
        </span>
      </button>
      {expanded && (
        <div className="mt-1 divide-y divide-white/[0.04] rounded-lg border border-white/[0.06] bg-white/[0.02]">
          {missions.map((mission) => (
            <MissionRow key={mission.id} mission={mission} />
          ))}
        </div>
      )}
    </div>
  );
}

function MissionRow({ mission }: { mission: ProjectMissionChip }) {
  const live = LIVE_STATUSES.has(mission.status);
  const problem = PROBLEM_STATUSES.has(mission.status);
  return (
    <Link
      href={`/control?mission=${mission.id}`}
      className="flex items-center gap-2.5 px-3 py-1.5 !no-underline transition-colors hover:bg-white/[0.03]"
    >
      {problem ? (
        <AlertTriangle className="h-3 w-3 shrink-0 text-amber-300/80" />
      ) : (
        <span
          className={cn(
            "h-1.5 w-1.5 shrink-0 rounded-full",
            live ? "animate-pulse bg-[rgb(var(--text)/0.65)]" : "bg-[rgb(var(--text)/0.25)]",
          )}
        />
      )}
      <span
        className={cn(
          "min-w-0 flex-1 truncate text-xs",
          live ? "text-white/80" : "text-white/50",
        )}
      >
        {mission.title || mission.id.slice(0, 8)}
      </span>
      {mission.github_pr && (
        <GitPullRequest className="h-3 w-3 shrink-0 text-white/30" />
      )}
      <span className="shrink-0 text-[10px] text-white/35">{mission.status}</span>
      <UpdateAge at={mission.updated_at} />
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
    .filter((line) => {
      const trimmed = line.trim();
      return (
        trimmed !== "[SILENT]" &&
        !trimmed.startsWith("[STATE_SIGNATURE:") &&
        !trimmed.startsWith("[CTRL:")
      );
    })
    .join("\n");
}

function UpdateEntry({
  update,
  defaultExpanded,
  replyToSessionId,
}: {
  update: ProjectDeliveryUpdate;
  defaultExpanded: boolean;
  /** Where a reply should go — the project's bound conversation when one is
   *  declared. Replying into the delivery's own session would land in a cron
   *  tick that has already ended. */
  replyToSessionId: string;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  return (
    <div>
      <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 transition-colors">
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="flex w-full items-center justify-between gap-2 text-left"
        >
          <span className="min-w-0 flex-1 truncate text-[13px] text-white/80">
            {stripMarkdown(update.headline) || "(untitled update)"}
          </span>
          <UpdateAge at={update.at} />
        </button>
        {update.blocker && (
          <p className="mt-1 flex items-start gap-1.5 text-xs text-white/60">
            <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0 text-amber-300/80" />
            Blocked by: {update.blocker}
          </p>
        )}
        {expanded && update.body && (
          <div className="mt-2 border-t border-white/[0.05] pt-2 text-sm">
            <MarkdownContent
              content={bodyWithoutHeadline(update.body, update.headline)}
            />
            <p className="mt-2 text-[11px] text-white/30">
              Origin conversation: {update.session_id}
            </p>
          </div>
        )}
        {expanded && <ReplyComposer sessionId={replyToSessionId} />}
      </div>
    </div>
  );
}

/** Pulse skeleton mirroring the two-pane layout while the overview loads. */
function BoardSkeleton() {
  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 pb-4 lg:grid-cols-[340px_minmax(0,1fr)] xl:grid-cols-[380px_minmax(0,1fr)]">
      <div className="animate-pulse overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02]">
        <div className="border-b border-white/[0.05] px-3 py-2">
          <div className="h-3 w-28 rounded bg-white/[0.06]" />
        </div>
        {Array.from({ length: 7 }).map((_, i) => (
          <div key={i} className="space-y-1.5 px-3 py-2.5">
            <div className="flex items-center justify-between">
              <div className="h-3.5 w-32 rounded bg-white/[0.06]" />
              <div className="h-2.5 w-10 rounded bg-white/[0.04]" />
            </div>
            <div className="h-2.5 w-48 rounded bg-white/[0.04]" />
          </div>
        ))}
      </div>
      <div className="hidden animate-pulse overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02] lg:block">
        <div className="space-y-3 border-b border-white/[0.06] px-5 py-4">
          <div className="flex items-center gap-3">
            <div className="h-5 w-40 rounded bg-white/[0.06]" />
            <div className="h-4 w-20 rounded-full bg-white/[0.04]" />
          </div>
          <div className="h-3 w-3/4 rounded bg-white/[0.04]" />
          <div className="flex gap-2">
            {Array.from({ length: 4 }).map((_, i) => (
              <div key={i} className="h-5 w-28 rounded-full bg-white/[0.04]" />
            ))}
          </div>
        </div>
        <div className="space-y-3 px-5 py-4">
          <div className="h-3 w-16 rounded bg-white/[0.05]" />
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className="h-16 rounded-lg border border-white/[0.05] bg-white/[0.02]" />
          ))}
        </div>
      </div>
    </div>
  );
}
