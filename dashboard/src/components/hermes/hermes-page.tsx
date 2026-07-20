"use client";

import Link from "next/link";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import useSWR from "swr";
import type { LucideIcon } from "lucide-react";
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  Bell,
  Bot,
  CheckCircle2,
  CircleOff,
  Clock3,
  Loader,
  MessageSquare,
  RadioTower,
  X,
} from "lucide-react";

import {
  getHermesAssistantStatus,
  getHermesMissionControl,
  type HermesMissionCard,
} from "@/lib/api";
import { HermesThread } from "@/components/hermes/hermes-thread";
import { AlertsFeed } from "@/components/hermes/alerts-feed";
import { RelativeTime } from "@/components/ui/relative-time";
import { cn } from "@/lib/utils";

export default function HermesPage() {
  const [openPanel, setOpenPanel] = useState<"missions" | "updates" | null>(
    null,
  );
  const missionsButtonRef = useRef<HTMLButtonElement | null>(null);
  const updatesButtonRef = useRef<HTMLButtonElement | null>(null);
  const { data: status, isLoading } = useSWR(
    "hermes-assistant-status",
    getHermesAssistantStatus,
    { refreshInterval: 30000, revalidateOnFocus: false },
  );
  const { data: control, isLoading: controlLoading } = useSWR(
    "hermes-mission-control",
    getHermesMissionControl,
    { refreshInterval: 15000, revalidateOnFocus: false },
  );

  const runtimeReady = Boolean(status?.service_active);
  const runtimeChecking = isLoading && !status;
  const runtime = control?.runtime;
  const sessionSources = Object.entries(control?.sessions.by_source ?? {})
    .sort((a, b) => b[1] - a[1])
    .slice(0, 4);
  const attentionCount = control?.needs_attention.length ?? 0;

  const closePanel = useCallback((restoreFocus = false) => {
    setOpenPanel((current) => {
      if (restoreFocus) {
        const button =
          current === "missions"
            ? missionsButtonRef.current
            : updatesButtonRef.current;
        window.setTimeout(() => button?.focus(), 0);
      }
      return null;
    });
  }, []);

  useEffect(() => {
    if (!openPanel) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") closePanel(true);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [closePanel, openPanel]);

  return (
    <div className="flex flex-col lg:h-screen lg:overflow-hidden">
      <div className="relative z-[60] flex items-center justify-between border-b border-white/[0.06] bg-[rgb(var(--background))] px-6 py-4">
        <div>
          <h1 className="text-xl font-semibold text-white">Hermes</h1>
          <p className="text-sm text-white/50">
            Your assistant for missions, follow-ups, and answers.
          </p>
        </div>
        <div className="flex items-center gap-3">
          {runtime?.model && (
            <span
              className="hidden rounded-md border border-white/[0.06] bg-white/[0.03] px-1.5 py-0.5 font-mono text-[10px] text-white/45 sm:inline"
              title="Model the Hermes runtime is configured to use"
            >
              {runtime.model}
            </span>
          )}
          <span className="flex items-center gap-1.5 text-xs text-white/45">
            <span
              className={cn(
                "h-1.5 w-1.5 rounded-full",
                runtimeReady ? "bg-emerald-400" : "bg-white/25",
              )}
            />
            {isLoading ? "checking..." : runtimeReady ? "online" : "offline"}
          </span>
          <button
            ref={missionsButtonRef}
            type="button"
            onClick={() =>
              setOpenPanel((current) =>
                current === "missions" ? null : "missions",
              )
            }
            className="relative inline-flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.03] px-2.5 py-1.5 text-xs text-white/60 transition-colors hover:bg-white/[0.06] hover:text-white/85"
          >
            <Activity className="h-3.5 w-3.5" />
            Missions
            {attentionCount > 0 && (
              <span className="inline-flex min-w-4 items-center justify-center rounded-full bg-amber-400/15 px-1 text-[10px] font-medium text-amber-200">
                {attentionCount}
              </span>
            )}
          </button>
          <button
            ref={updatesButtonRef}
            type="button"
            aria-label="Open mission updates"
            onClick={() =>
              setOpenPanel((current) =>
                current === "updates" ? null : "updates",
              )
            }
            className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-white/[0.06] bg-white/[0.03] text-white/50 transition-colors hover:bg-white/[0.06] hover:text-white/80"
            title="Mission updates"
          >
            <Bell className="h-3.5 w-3.5" />
          </button>
          <Link
            href="/assistant"
            className="hidden items-center gap-1 text-xs text-white/45 transition-colors hover:text-white/75 sm:inline-flex"
          >
            Manage <ArrowUpRight className="h-3 w-3" />
          </Link>
        </div>
      </div>

      <main className="mx-auto min-h-0 w-full max-w-5xl flex-1 overflow-hidden border-x border-white/[0.04]">
        <div className="h-full min-h-[560px]">
          {runtimeReady || runtimeChecking ? (
            <HermesThread className="h-full" />
          ) : (
            <HermesOfflinePanel />
          )}
        </div>
      </main>

      {openPanel === "missions" && (
        <Drawer title="Mission control" onClose={() => closePanel()}>
          {controlLoading && !control ? (
            <div className="flex items-center gap-2 text-sm text-white/45">
              <Loader className="h-4 w-4 animate-spin" /> Loading mission
              control...
            </div>
          ) : (
            <MissionControlContent
              control={control}
              runtimeReady={runtimeReady}
              runtimeModel={runtime?.model ?? status?.model ?? null}
              sessionSources={sessionSources}
            />
          )}
        </Drawer>
      )}

      {openPanel === "updates" && (
        <Drawer title="Mission updates" onClose={() => closePanel()}>
          <AlertsFeed />
        </Drawer>
      )}
    </div>
  );
}

function Drawer({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <div className="fixed inset-0 z-50 flex justify-end">
      <button
        type="button"
        aria-label={`Close ${title}`}
        onClick={onClose}
        className="absolute inset-0 bg-black/55 backdrop-blur-sm"
      />
      <section
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="relative flex h-full w-full max-w-[560px] animate-fade-in flex-col border-l border-white/[0.08] bg-[rgb(var(--background-elevated))] shadow-2xl"
      >
        <div className="flex h-16 shrink-0 items-center justify-between border-b border-white/[0.06] px-5">
          <h2 className="text-sm font-semibold text-white/90">{title}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={`Close ${title}`}
            className="inline-flex h-8 w-8 items-center justify-center rounded-lg text-white/45 transition-colors hover:bg-white/[0.06] hover:text-white/80"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">{children}</div>
      </section>
    </div>
  );
}

function MissionControlContent({
  control,
  runtimeReady,
  runtimeModel,
  sessionSources,
}: {
  control: Awaited<ReturnType<typeof getHermesMissionControl>> | undefined;
  runtimeReady: boolean;
  runtimeModel: string | null;
  sessionSources: [string, number][];
}) {
  const runtime = control?.runtime;
  return (
    <div className="space-y-4">
      <section className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Metric
          icon={Activity}
          label="Active"
          value={String(control?.active.length ?? 0)}
          detail={`${control?.mission_status_counts.active ?? 0} running / ${control?.mission_status_counts.waiting_background ?? 0} background`}
        />
        <Metric
          icon={AlertTriangle}
          label="Needs Attention"
          value={String(control?.needs_attention.length ?? 0)}
          detail={
            control?.failures[0]
              ? `${control.failures[0].class}: ${control.failures[0].count}`
              : "No grouped failures"
          }
          tone={(control?.needs_attention.length ?? 0) > 0 ? "warn" : "ok"}
        />
        <Metric
          icon={MessageSquare}
          label="Hermes Sessions"
          value={String(control?.sessions.total ?? 0)}
          detail={`${control?.sessions.messages ?? 0} messages / ${control?.sessions.tool_calls ?? 0} tools`}
        />
      </section>

      <RuntimePanel
        serviceActive={runtimeReady}
        model={runtimeModel}
        baseUrl={runtime?.base_url ?? null}
        expectedBaseUrl={runtime?.expected_base_url ?? null}
        usesProxy={runtime?.uses_sandboxed_proxy ?? false}
        notes={runtime?.notes ?? []}
      />

      {(sessionSources.length > 0 || control?.remote_nodes.enabled) && (
        <Panel title="Watchers" icon={RadioTower}>
          <div className="space-y-2">
            {sessionSources.map(([source, count]) => (
              <div
                key={source}
                className="flex items-center justify-between text-xs"
              >
                <span className="capitalize text-white/65">{source}</span>
                <span className="font-mono text-white/40">{count}</span>
              </div>
            ))}
            {control?.remote_nodes.enabled && (
              <div className="border-t border-white/[0.06] pt-2 text-xs text-white/40">
                Remote nodes enabled
                {control.remote_nodes.configured_nodes
                  ? ` / ${control.remote_nodes.configured_nodes} configured`
                  : ""}
              </div>
            )}
          </div>
        </Panel>
      )}

      <MissionList
        title="Now"
        icon={Clock3}
        missions={control?.active ?? []}
        empty="No active supervised missions."
      />
      {(control?.needs_attention.length ?? 0) > 0 && (
        <MissionList
          title="Needs Attention"
          icon={AlertTriangle}
          missions={control?.needs_attention ?? []}
          empty=""
        />
      )}
      {(control?.handled_recently.length ?? 0) > 0 && (
        <MissionList
          title="Handled Recently"
          icon={CheckCircle2}
          missions={control?.handled_recently ?? []}
          empty=""
        />
      )}
    </div>
  );
}

function HermesOfflinePanel() {
  return (
    <div className="flex h-full min-h-[360px] items-center justify-center p-6">
      <div className="max-w-xs text-center">
        <CircleOff className="mx-auto h-8 w-8 text-white/25" />
        <p className="mt-3 text-sm text-white/70">Hermes chat is offline.</p>
        <p className="mt-1 text-xs text-white/40">
          Mission control remains available so the runtime can be diagnosed.
        </p>
        <Link
          href="/assistant"
          className="mt-4 inline-flex items-center gap-1 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-indigo-600"
        >
          Open Assistant setup <ArrowUpRight className="h-3 w-3" />
        </Link>
      </div>
    </div>
  );
}

function Metric({
  icon: Icon,
  label,
  value,
  detail,
  tone = "neutral",
}: {
  icon: LucideIcon;
  label: string;
  value: string;
  detail: string;
  tone?: "neutral" | "ok" | "warn";
}) {
  return (
    <div className="rounded-lg border border-white/[0.06] bg-white/[0.025] p-3">
      <div className="flex items-center gap-2 text-xs text-white/45">
        <Icon className={cn("h-3.5 w-3.5", tone === "warn" ? "text-amber-300" : tone === "ok" ? "text-emerald-300" : "text-primary")} />
        {label}
      </div>
      <div className="mt-2 text-2xl font-semibold text-white">{value}</div>
      <div className="mt-1 truncate text-xs text-white/40">{detail}</div>
    </div>
  );
}

function Panel({
  title,
  icon: Icon,
  children,
}: {
  title: string;
  icon: LucideIcon;
  children: ReactNode;
}) {
  return (
    <section className="rounded-lg border border-white/[0.06] bg-white/[0.025] p-3">
      <div className="mb-3 flex items-center gap-2 text-xs font-medium text-white/70">
        <Icon className="h-3.5 w-3.5 text-primary" />
        {title}
      </div>
      {children}
    </section>
  );
}

function RuntimePanel({
  serviceActive,
  model,
  baseUrl,
  expectedBaseUrl,
  usesProxy,
  notes,
}: {
  serviceActive: boolean;
  model: string | null;
  baseUrl: string | null;
  expectedBaseUrl: string | null;
  usesProxy: boolean;
  notes: string[];
}) {
  const nativeCodex = isNativeCodexRuntime(model, baseUrl);
  const routingOk = usesProxy || nativeCodex;
  const routingText = usesProxy
    ? "sandboxed.sh proxy"
    : nativeCodex
      ? "native OpenAI Codex"
      : "direct or unknown";

  return (
    <Panel title="Runtime Routing" icon={Bot}>
      <div className="space-y-2 text-xs">
        <StatusLine label="Runtime" ok={serviceActive} text={serviceActive ? "service active" : "service offline"} />
        <StatusLine label="Routing" ok={routingOk} text={routingText} />
        <div className="grid grid-cols-[72px_minmax(0,1fr)] gap-2 text-white/45">
          <span>Model</span>
          <span className="truncate font-mono text-white/70" title={model ?? "unknown"}>
            {model ?? "unknown"}
          </span>
        </div>
        <div className="grid grid-cols-[72px_minmax(0,1fr)] gap-2 text-white/35">
          <span>Base URL</span>
          <span className="truncate font-mono" title={baseUrl ?? expectedBaseUrl ?? undefined}>
            {baseUrl ?? "unknown"}
          </span>
        </div>
        {notes.slice(0, 3).map((note) => (
          <div key={note} className="rounded-md bg-amber-500/10 px-2 py-1 text-amber-200/80">
            {note}
          </div>
        ))}
      </div>
    </Panel>
  );
}

function isNativeCodexRuntime(model: string | null, baseUrl: string | null) {
  const normalizedModel = model?.toLowerCase() ?? "";
  const normalizedBaseUrl = baseUrl?.toLowerCase() ?? "";

  return (
    normalizedModel.includes("openai-codex") ||
    normalizedModel.includes("gpt-5.6") ||
    normalizedModel.includes("gpt-5.5") ||
    normalizedBaseUrl.includes("chatgpt.com/backend-api/codex")
  );
}

function StatusLine({ label, ok, text }: { label: string; ok: boolean; text: string }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-white/45">{label}</span>
      <span className={cn("flex min-w-0 items-center gap-1.5 text-right", ok ? "text-emerald-300/80" : "text-amber-300/80")}>
        <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", ok ? "bg-emerald-400" : "bg-amber-300")} />
        {text}
      </span>
    </div>
  );
}

function MissionList({
  title,
  icon: Icon,
  missions,
  empty,
}: {
  title: string;
  icon: LucideIcon;
  missions: HermesMissionCard[];
  empty: string;
}) {
  return (
    <Panel title={title} icon={Icon}>
      <div className="divide-y divide-white/[0.04]">
        {missions.length === 0 ? (
          <p className="py-2 text-xs text-white/35">{empty}</p>
        ) : (
          missions.map((mission) => <MissionRow key={mission.id} mission={mission} />)
        )}
      </div>
    </Panel>
  );
}

function MissionRow({ mission }: { mission: HermesMissionCard }) {
  const ts = new Date(mission.last_activity_at ?? mission.updated_at);
  return (
    <Link
      href={`/control?mission=${mission.id}`}
      className="block py-2 transition-colors hover:bg-white/[0.03]"
    >
      <div className="flex min-w-0 items-center gap-2 px-1">
        <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", dotForStatus(mission.status))} />
        <span className="min-w-0 flex-1 truncate text-sm text-white/80">
          {mission.title || shortMission(mission.id)}
        </span>
        {!Number.isNaN(ts.getTime()) && (
          <RelativeTime date={ts} className="shrink-0 text-[10px] text-white/30" />
        )}
      </div>
      <div className="mt-0.5 flex min-w-0 items-center gap-2 px-1 pl-3.5 text-[11px] text-white/40">
        <span className="shrink-0">{mission.status}</span>
        <span className="shrink-0">{mission.backend}{mission.model_effort ? `/${mission.model_effort}` : ""}</span>
        {mission.workspace_name && <span className="truncate">{mission.workspace_name}</span>}
        {mission.attention && <span className="truncate text-amber-300/70">{mission.attention}</span>}
      </div>
    </Link>
  );
}

function shortMission(id: string) {
  return id.slice(0, 8);
}

function dotForStatus(status: string) {
  switch (status) {
    case "active":
      return "bg-sky-400";
    case "pending":
    case "waiting_background":
      return "bg-amber-300";
    case "acknowledged":
    case "completed":
      return "bg-emerald-400";
    case "failed":
    case "blocked":
    case "interrupted":
    case "not_feasible":
      return "bg-red-400";
    default:
      return "bg-white/30";
  }
}
