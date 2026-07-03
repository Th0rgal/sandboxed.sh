"use client";

import Link from "next/link";
import useSWR from "swr";
import { ArrowUpRight, CircleOff } from "lucide-react";

import { getHermesAssistantStatus } from "@/lib/api/assistant";
import { HermesThread } from "@/components/hermes/hermes-thread";
import { AlertsFeed } from "@/components/hermes/alerts-feed";

/**
 * Landing page: chat with the Hermes assistant, with the cross-mission
 * alerts feed on a right rail. Management of the Hermes runtime itself
 * (gateways, memory, skills) stays on /assistant.
 */
export default function HermesPage() {
  const { data: status, isLoading } = useSWR(
    "hermes-assistant-status",
    getHermesAssistantStatus,
    { refreshInterval: 30000, revalidateOnFocus: false },
  );

  const runtimeReady = Boolean(status?.service_active);

  return (
    <div className="flex flex-col lg:h-screen lg:flex-row lg:overflow-hidden">
      {/* Chat column */}
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="flex items-center justify-between border-b border-white/[0.06] px-6 py-4">
          <div>
            <h1 className="text-xl font-semibold text-white">Hermes</h1>
            <p className="text-sm text-white/50">
              Your assistant, wired into every mission.
            </p>
          </div>
          <div className="flex items-center gap-3">
            <span className="flex items-center gap-1.5 text-xs text-white/45">
              <span
                className={
                  runtimeReady
                    ? "h-1.5 w-1.5 rounded-full bg-emerald-400"
                    : "h-1.5 w-1.5 rounded-full bg-white/25"
                }
              />
              {isLoading ? "checking…" : runtimeReady ? "online" : "offline"}
            </span>
            <Link
              href="/assistant"
              className="inline-flex items-center gap-1 text-xs text-white/45 transition-colors hover:text-white/75"
            >
              Manage <ArrowUpRight className="h-3 w-3" />
            </Link>
          </div>
        </div>

        {runtimeReady || isLoading ? (
          <HermesThread className="flex-1" />
        ) : (
          <div className="flex flex-1 items-center justify-center p-8">
            <div className="max-w-md text-center">
              <CircleOff className="mx-auto h-8 w-8 text-white/25" />
              <p className="mt-3 text-sm text-white/70">
                The Hermes runtime is not running.
              </p>
              <p className="mt-1 text-xs text-white/40">
                Install or start it from the Assistant page, then come back —
                this page talks to the same Hermes that answers you on
                Telegram.
              </p>
              <Link
                href="/assistant"
                className="mt-4 inline-flex items-center gap-1 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-indigo-600"
              >
                Open Assistant setup <ArrowUpRight className="h-3 w-3" />
              </Link>
            </div>
          </div>
        )}
      </div>

      {/* Alerts rail */}
      <div className="flex w-full flex-col gap-4 overflow-y-auto border-t border-white/[0.06] p-4 lg:h-screen lg:w-80 lg:border-l lg:border-t-0">
        <AlertsFeed />
      </div>
    </div>
  );
}
