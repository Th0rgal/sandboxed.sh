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
  GitPullRequest,
  Inbox,
  Loader,
  Pause,
  Play,
  Search,
  Send,
  Trash2,
} from "lucide-react";

import {
  getProjectsOverview,
  getProjectUpdates,
  postProjectAction,
  type ProjectAction,
  type ProjectBucket,
  type ProjectDeliveryUpdate,
  type ProjectMissionChip,
  type ProjectRow,
} from "@/lib/api/projects";
import { hermesChatStream } from "@/lib/api/hermes";
import { MarkdownContent } from "@/components/markdown-content";
import { RelativeTime } from "@/components/ui/relative-time";
import { cn } from "@/lib/utils";

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
                <span className="flex items-center gap-1 text-amber-300">
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
            ref={listRef}
            className={cn(
              "min-h-0 overflow-y-auto rounded-xl border border-white/[0.06] bg-white/[0.015]",
              mobileDetail && "hidden lg:block",
            )}
          >
            {sections.map((section) => (
              <div key={section.bucket}>
                <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-white/[0.05] bg-[rgb(var(--background))]/95 px-3 py-1.5 backdrop-blur">
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
              <div className="border-t border-white/[0.05] px-3 py-2.5">
                <p className="mb-1 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-white/35">
                  <Inbox className="h-3 w-3" /> Unrouted ({unrouted.length})
                </p>
                {unrouted.slice(0, 4).map((update, index) => (
                  <div
                    key={`${update.session_id}-${update.at}-${index}`}
                    className="flex min-w-0 items-center gap-2 py-0.5 text-[11px] text-white/40"
                  >
                    <span className="min-w-0 flex-1 truncate">
                      {update.headline || "(untitled)"}
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
                Select a project to see its timeline.
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
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
        "group relative flex w-full items-stretch gap-2.5 rounded-md px-3 py-2 text-left transition-colors",
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
            {project.slug}
          </span>
          {project.latest_update && <UpdateAge at={project.latest_update.at} />}
        </span>
        {!quiet && (
          <span className="mt-0.5 flex min-w-0 items-center gap-2 text-[11px] text-white/40">
            {live > 0 && (
              <span className="flex shrink-0 items-center gap-1 text-white/55">
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-white/60" />
                {live}
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
    </button>
  );
}

/** Icon button for the detail-pane action bar. */
function ActionButton({
  icon: Icon,
  label,
  onClick,
  busy,
  danger,
}: {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  busy?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      title={label}
      className={cn(
        "flex items-center gap-1.5 rounded-lg border px-2 py-1 text-[11px] transition-colors disabled:opacity-50",
        danger
          ? "border-amber-400/25 bg-amber-500/10 text-amber-300 hover:border-amber-400/50"
          : "border-white/[0.08] bg-white/[0.03] text-white/50 hover:border-white/20 hover:text-white/80",
      )}
    >
      {busy ? (
        <Loader className="h-3 w-3 animate-spin" />
      ) : (
        <Icon className="h-3 w-3" />
      )}
      {label}
    </button>
  );
}

function ProjectActions({ project }: { project: ProjectRow }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<ProjectAction | null>(null);
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

  return (
    <div className="flex flex-wrap items-center gap-1.5">
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
          placeholder={`Reply in session ${sessionId.slice(0, 14)}…`}
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
              Reply from session {sessionId.slice(0, 14)}
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
          <h2 className="min-w-0 truncate text-base font-semibold text-white/90">
            {project.slug}
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
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function MissionMiniChip({ mission }: { mission: ProjectMissionChip }) {
  const live = LIVE_STATUSES.has(mission.status);
  const problem = PROBLEM_STATUSES.has(mission.status);
  return (
    <Link
      href={`/control?mission=${mission.id}`}
      onClick={(event) => event.stopPropagation()}
      title={`${mission.title ?? mission.id} — ${mission.status}`}
      className={cn(
        "inline-flex max-w-[200px] items-center gap-1.5 rounded-full border border-white/[0.09] bg-white/[0.03] px-2 py-0.5 text-[11px] !no-underline transition-colors hover:border-white/25",
        live ? "text-white/75" : "text-white/45",
      )}
    >
      {problem ? (
        <AlertTriangle className="h-3 w-3 shrink-0 text-amber-300/80" />
      ) : (
        <span
          className={cn(
            "h-1.5 w-1.5 shrink-0 rounded-full",
            live ? "animate-pulse bg-white/70" : "bg-white/25",
          )}
        />
      )}
      <span className="truncate">
        {mission.title || mission.id.slice(0, 8)}
      </span>
      {mission.github_pr && (
        <GitPullRequest className="h-3 w-3 shrink-0 opacity-60" />
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
    .filter((line) => {
      const trimmed = line.trim();
      return trimmed !== "[SILENT]" && !trimmed.startsWith("[STATE_SIGNATURE:");
    })
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
    <div>
      <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 transition-colors">
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="flex w-full items-center justify-between gap-2 text-left"
        >
          <span className="min-w-0 flex-1 truncate text-[13px] text-white/80">
            {update.headline || "(untitled update)"}
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
              Origin session: {update.session_id}
            </p>
          </div>
        )}
        {expanded && <ReplyComposer sessionId={update.session_id} />}
      </div>
    </div>
  );
}

/** Pulse skeleton mirroring the two-pane layout while the overview loads. */
function BoardSkeleton() {
  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 pb-4 lg:grid-cols-[340px_minmax(0,1fr)] xl:grid-cols-[380px_minmax(0,1fr)]">
      <div className="animate-pulse overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.015]">
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
      <div className="hidden animate-pulse overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.015] lg:block">
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
