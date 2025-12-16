"use client";

import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { getHealth } from "@/lib/api";

interface ConnectionItem {
  name: string;
  status: "connected" | "disconnected" | "checking";
  latency?: number;
}

export function ConnectionStatus() {
  const [connections, setConnections] = useState<ConnectionItem[]>([
    { name: "Dashboard → API", status: "checking" },
    { name: "API → LLM", status: "checking" },
  ]);
  const [overallStatus, setOverallStatus] = useState<
    "all" | "partial" | "none"
  >("partial");

  useEffect(() => {
    const checkConnections = async () => {
      const start = Date.now();
      try {
        await getHealth();
        const latency = Date.now() - start;
        setConnections([
          { name: "Dashboard → API", status: "connected", latency },
          { name: "API → LLM", status: "connected" },
        ]);
        setOverallStatus("all");
      } catch {
        setConnections([
          { name: "Dashboard → API", status: "disconnected" },
          { name: "API → LLM", status: "disconnected" },
        ]);
        setOverallStatus("none");
      }
    };

    checkConnections();
    const interval = setInterval(checkConnections, 5000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div>
      <h3 className="mb-4 text-sm font-medium text-white">Connection Status</h3>

      <div className="space-y-3">
        {connections.map((conn, i) => (
          <div key={i} className="flex items-center justify-between">
            <div>
              <p className="text-xs text-white/50">{conn.name}</p>
              {conn.latency !== undefined && (
                <p className="text-lg font-light text-white tabular-nums">
                  {conn.latency}
                  <span className="text-xs text-white/40 ml-0.5">ms</span>
                </p>
              )}
            </div>
            <span
              className={cn(
                "h-2 w-2 rounded-full",
                conn.status === "connected" && "bg-emerald-400",
                conn.status === "disconnected" && "bg-red-400",
                conn.status === "checking" && "bg-amber-400 animate-pulse"
              )}
            />
          </div>
        ))}
      </div>

      <div className="mt-4 flex items-center justify-between border-t border-white/[0.06] pt-4">
        <span className="text-xs text-white/40">All Systems</span>
        <span
          className={cn(
            "text-xs font-medium",
            overallStatus === "all" && "text-emerald-400",
            overallStatus === "partial" && "text-amber-400",
            overallStatus === "none" && "text-red-400"
          )}
        >
          {overallStatus === "all" && "Operational"}
          {overallStatus === "partial" && "Partial"}
          {overallStatus === "none" && "Offline"}
        </span>
      </div>
    </div>
  );
}
