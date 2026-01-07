"use client";

import { useCallback, useEffect, useState, useMemo } from "react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import {
  listMcps,
  listTools,
  addMcp,
  removeMcp,
  enableMcp,
  disableMcp,
  refreshMcp,
  refreshAllMcps,
  type McpServerState,
  type McpStatus,
  type ToolInfo,
} from "@/lib/api";
import { ShimmerCard } from "@/components/ui/shimmer";
import { CopyButton } from "@/components/ui/copy-button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import {
  Plus,
  RefreshCw,
  Trash2,
  X,
  CheckCircle,
  XCircle,
  AlertTriangle,
  Power,
  ChevronLeft,
  Plug,
  Settings,
  Search,
} from "lucide-react";

type TabType = "installed" | "tools";

const statusConfig: Record<
  McpStatus,
  { color: string; bg: string; label: string }
> = {
  connected: {
    color: "text-emerald-400",
    bg: "bg-emerald-500/10",
    label: "Connected",
  },
  disconnected: {
    color: "text-white/40",
    bg: "bg-white/[0.04]",
    label: "Disconnected",
  },
  error: { color: "text-red-400", bg: "bg-red-500/10", label: "Error" },
  disabled: {
    color: "text-white/40",
    bg: "bg-white/[0.04]",
    label: "Disabled",
  },
};

function Toggle({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: () => void;
}) {
  return (
    <button
      onClick={(e) => {
        e.stopPropagation();
        onChange();
      }}
      className={cn(
        "relative h-5 w-9 rounded-full transition-colors",
        checked ? "bg-emerald-500" : "bg-white/10"
      )}
    >
      <span
        className={cn(
          "absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all",
          checked ? "left-[18px]" : "left-0.5"
        )}
      />
    </button>
  );
}

function McpCard({
  mcp,
  onSelect,
  isSelected,
}: {
  mcp: McpServerState;
  onSelect: (mcp: McpServerState | null) => void;
  isSelected: boolean;
}) {
  const status = statusConfig[mcp.status];

  const handleSelect = () => onSelect(isSelected ? null : mcp);

  return (
    <div
      role="button"
      tabIndex={0}
      aria-pressed={isSelected}
      onClick={handleSelect}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          handleSelect();
        }
      }}
      className={cn(
        "w-full rounded-xl p-4 text-left transition-all cursor-pointer",
        "bg-white/[0.02] border hover:bg-white/[0.04] focus:outline-none focus:ring-1 focus:ring-indigo-500/40",
        isSelected
          ? "border-indigo-500/50 ring-1 ring-indigo-500/30"
          : "border-white/[0.04] hover:border-white/[0.08]"
      )}
    >
      {/* Header with icon and status */}
      <div className="flex items-start gap-3 mb-3">
        <div
          className={cn(
            "flex h-10 w-10 items-center justify-center rounded-xl",
            mcp.enabled ? "bg-indigo-500/10" : "bg-white/[0.04]"
          )}
        >
          <Plug
            className={cn(
              "h-5 w-5",
              mcp.enabled ? "text-indigo-400" : "text-white/40"
            )}
          />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="font-medium text-white truncate">{mcp.name}</h3>
            <span
              className={cn(
                "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[10px] font-medium",
                status.bg,
                status.color
              )}
            >
              <span
                className={cn(
                  "h-1.5 w-1.5 rounded-full",
                  mcp.status === "connected"
                    ? "bg-emerald-400"
                    : mcp.status === "error"
                    ? "bg-red-400"
                    : "bg-white/40"
                )}
              />
              {status.label}
            </span>
          </div>
          <div className="flex items-center gap-1 group">
            <p className="text-xs text-white/40 truncate">{mcp.endpoint}</p>
            <CopyButton
              text={mcp.endpoint}
              showOnHover
              label="Copied endpoint"
            />
          </div>
        </div>
      </div>

      {/* Tags */}
      <div className="flex flex-wrap gap-1 mb-3">
        {mcp.tools.slice(0, 3).map((tool) => (
          <span key={tool} className="tag">
            {tool}
          </span>
        ))}
        {mcp.tools.length > 3 && (
          <span className="tag">+{mcp.tools.length - 3}</span>
        )}
        {mcp.tools.length === 0 && (
          <span className="text-[10px] text-white/30">No tools</span>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <span className="text-[10px] text-white/30">
          {mcp.tool_calls} calls
        </span>
        <Toggle checked={mcp.enabled} onChange={() => {}} />
      </div>
    </div>
  );
}

function McpDetailPanel({
  mcp,
  onClose,
  onToggle,
  onRefresh,
  onConfigure,
  onDelete,
}: {
  mcp: McpServerState;
  onClose: () => void;
  onToggle: () => void;
  onRefresh: () => void;
  onConfigure: () => void;
  onDelete: () => void;
}) {
  const accuracy =
    mcp.tool_calls + mcp.tool_errors > 0
      ? ((mcp.tool_calls / (mcp.tool_calls + mcp.tool_errors)) * 100).toFixed(1)
      : "100.0";

  // Handle Escape key to close panel
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  return (
    <>
      {/* Backdrop */}
      <div
        className="fixed inset-0 z-40 bg-black/40 backdrop-blur-sm animate-fade-in"
        onClick={onClose}
      />
      {/* Panel */}
      <div
        className="fixed right-0 top-0 z-50 h-full w-96 flex flex-col glass-panel border-l border-white/[0.06] animate-slide-in-right"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-start justify-between border-b border-white/[0.06] p-4">
          <div className="flex items-center gap-3">
            <button
              onClick={(e) => {
                e.stopPropagation();
                onClose();
              }}
              className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
            >
              <ChevronLeft className="h-5 w-5" />
            </button>
            <div>
              <div className="flex items-center gap-2">
                <h2 className="text-lg font-semibold text-white">{mcp.name}</h2>
                {mcp.version && <span className="tag">v{mcp.version}</span>}
              </div>
              <p className="text-xs text-white/40">{mcp.endpoint}</p>
            </div>
          </div>
          <button
            onClick={(e) => {
              e.stopPropagation();
              onClose();
            }}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-6">
          {/* Active toggle */}
          <div className="flex items-center justify-between rounded-xl bg-white/[0.02] border border-white/[0.04] p-4">
            <div className="flex items-center gap-3">
              <div
                className={cn(
                  "flex h-10 w-10 items-center justify-center rounded-xl",
                  !mcp.enabled
                    ? "bg-white/[0.04]"
                    : mcp.status === "connected"
                    ? "bg-emerald-500/10"
                    : mcp.status === "error"
                    ? "bg-red-500/10"
                    : "bg-yellow-500/10"
                )}
              >
                <Power
                  className={cn(
                    "h-5 w-5",
                    !mcp.enabled
                      ? "text-white/40"
                      : mcp.status === "connected"
                      ? "text-emerald-400"
                      : mcp.status === "error"
                      ? "text-red-400"
                      : "text-yellow-400"
                  )}
                />
              </div>
              <div>
                <p className="font-medium text-white">
                  {!mcp.enabled
                    ? "Module Disabled"
                    : mcp.status === "connected"
                    ? "Module Active"
                    : mcp.status === "error"
                    ? "Module Error"
                    : "Module Pending"}
                </p>
                <p className="text-xs text-white/40">
                  {!mcp.enabled
                    ? "Paused"
                    : mcp.status === "connected"
                    ? "Running and monitoring"
                    : mcp.status === "error"
                    ? "Connection failed"
                    : "Connecting..."}
                </p>
              </div>
            </div>
            <Toggle checked={mcp.enabled} onChange={onToggle} />
          </div>

          {/* Stats */}
          <div className="grid grid-cols-3 gap-3">
            <div className="stat-panel text-center">
              <p className="stat-label flex items-center justify-center gap-1">
                <AlertTriangle className="h-3 w-3" />
                CALLS
              </p>
              <p className="text-2xl font-light text-white tabular-nums">
                {mcp.tool_calls}
              </p>
            </div>
            <div className="stat-panel text-center">
              <p className="stat-label flex items-center justify-center gap-1 text-red-400">
                <XCircle className="h-3 w-3" />
                ERRORS
              </p>
              <p className="text-2xl font-light text-white tabular-nums">
                {mcp.tool_errors}
              </p>
            </div>
            <div className="stat-panel text-center">
              <p className="stat-label flex items-center justify-center gap-1 text-emerald-400">
                <CheckCircle className="h-3 w-3" />
                ACCURACY
              </p>
              <p className="text-2xl font-light text-emerald-400 tabular-nums">
                {accuracy}%
              </p>
            </div>
          </div>

          {/* About */}
          <div>
            <h3 className="text-[10px] uppercase tracking-wider text-white/40 mb-2">
              About
            </h3>
            <p className="text-sm text-white/80">
              {mcp.description || `Module running at ${mcp.endpoint}`}
            </p>
            {mcp.last_connected_at && (
              <p className="mt-2 text-xs text-white/40">
                Last updated: {new Date(mcp.last_connected_at).toLocaleString()}
              </p>
            )}
            {mcp.error && (
              <div className="mt-2 rounded-lg bg-red-500/10 border border-red-500/20 p-3">
                <p className="text-xs text-red-400">Error: {mcp.error}</p>
              </div>
            )}
          </div>

          {/* Active tools */}
          <div>
            <h3 className="text-[10px] uppercase tracking-wider text-white/40 mb-2">
              Active Checks ({mcp.tools.length})
            </h3>
            <div className="space-y-2">
              {mcp.tools.length === 0 ? (
                <p className="text-sm text-white/40">No tools discovered</p>
              ) : (
                mcp.tools.map((tool) => (
                  <div
                    key={tool}
                    className="flex items-center justify-between rounded-lg bg-white/[0.02] border border-white/[0.04] px-3 py-2.5"
                  >
                    <div className="flex items-center gap-2">
                      <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                      <span className="text-sm text-white">{tool}</span>
                    </div>
                    <span className="text-xs text-white/40">Active</span>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between border-t border-white/[0.06] p-4">
          <span className="text-xs text-white/30">Last updated recently</span>
          <div className="flex items-center gap-2">
            <button
              onClick={onRefresh}
              className="flex items-center gap-1.5 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-3 py-1.5 text-xs text-white/80 transition-colors"
            >
              <RefreshCw className="h-3 w-3" />
              Refresh
            </button>
            <button
              onClick={onConfigure}
              className="flex items-center gap-1.5 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-3 py-1.5 text-xs text-white/80 transition-colors"
            >
              <Settings className="h-3 w-3" />
              Configure
            </button>
            <button
              onClick={onDelete}
              className="flex items-center gap-1.5 rounded-lg bg-red-500/10 hover:bg-red-500/20 border border-red-500/20 px-3 py-1.5 text-xs text-red-400 transition-colors"
            >
              <Trash2 className="h-3 w-3" />
              Remove
            </button>
          </div>
        </div>
      </div>
    </>
  );
}

function AddMcpModal({
  onClose,
  onAdd,
}: {
  onClose: () => void;
  onAdd: (data: {
    name: string;
    endpoint: string;
    description?: string;
  }) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [description, setDescription] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Handle Escape key to close modal
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !endpoint.trim()) return;

    setLoading(true);
    setError(null);

    try {
      await onAdd({
        name: name.trim(),
        endpoint: endpoint.trim(),
        description: description.trim() || undefined,
      });
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to add MCP");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-fade-in">
      <div className="w-full max-w-md rounded-2xl glass-panel border border-white/[0.08] p-6 animate-slide-up">
        <div className="mb-6 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-white">Add MCP Server</h2>
          <button
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="space-y-4">
            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Name
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g., Supabase MCP"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Endpoint URL
              </label>
              <input
                type="url"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder="http://127.0.0.1:4011"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Description (optional)
              </label>
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What does this MCP do?"
                rows={2}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
              />
            </div>

            {error && (
              <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3">
                <p className="text-sm text-red-400">{error}</p>
              </div>
            )}
          </div>

          <div className="mt-6 flex justify-end gap-3">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-4 py-2.5 text-sm text-white/80 transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading || !name.trim() || !endpoint.trim()}
              className="rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2.5 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {loading ? "Adding..." : "Add MCP"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

function ConfigureMcpModal({
  mcp,
  onClose,
  onSave,
}: {
  mcp: McpServerState;
  onClose: () => void;
  onSave: (data: {
    name: string;
    endpoint: string;
    description?: string;
  }) => Promise<void>;
}) {
  const [name, setName] = useState(mcp.name);
  const [endpoint, setEndpoint] = useState(mcp.endpoint);
  const [description, setDescription] = useState(mcp.description || "");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Handle Escape key to close modal
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !endpoint.trim()) return;

    setLoading(true);
    setError(null);

    try {
      await onSave({
        name: name.trim(),
        endpoint: endpoint.trim(),
        description: description.trim() || undefined,
      });
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to update MCP");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-fade-in">
      <div className="w-full max-w-md rounded-2xl glass-panel border border-white/[0.08] p-6 animate-slide-up">
        <div className="mb-6 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-white">
            Configure {mcp.name}
          </h2>
          <button
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="space-y-4">
            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Name
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g., Supabase MCP"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Endpoint URL
              </label>
              <input
                type="url"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder="https://mcp.supabase.com/mcp"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Description (optional)
              </label>
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What does this MCP do?"
                rows={2}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
              />
            </div>

            {/* Connection status info */}
            <div className="rounded-lg bg-white/[0.02] border border-white/[0.04] p-3">
              <p className="text-xs text-white/60 mb-2">Connection Status</p>
              <div className="flex items-center gap-2">
                <span
                  className={cn(
                    "h-2 w-2 rounded-full",
                    mcp.status === "connected"
                      ? "bg-emerald-400"
                      : mcp.status === "error"
                      ? "bg-red-400"
                      : "bg-white/40"
                  )}
                />
                <span className="text-sm text-white capitalize">
                  {mcp.status}
                </span>
              </div>
              {mcp.error && (
                <p className="mt-2 text-xs text-red-400">{mcp.error}</p>
              )}
            </div>

            {error && (
              <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3">
                <p className="text-sm text-red-400">{error}</p>
              </div>
            )}
          </div>

          <div className="mt-6 flex justify-end gap-3">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-4 py-2.5 text-sm text-white/80 transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading || !name.trim() || !endpoint.trim()}
              className="rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2.5 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {loading ? "Saving..." : "Save Changes"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

function ToolsTab({ tools }: { tools: ToolInfo[] }) {
  const mcpTools = tools.filter(
    (t) => typeof t.source === "object" && "mcp" in t.source
  );

  return (
    <div className="space-y-6">
      <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-4 text-sm text-white/60">
        Tools are provided by MCP servers and surfaced to OpenCode. Enable or
        disable an MCP in the Installed tab to control availability.
      </div>

      <div>
        <h3 className="mb-3 text-sm font-medium text-white">
          MCP Tools ({mcpTools.length})
        </h3>
        {mcpTools.length === 0 ? (
          <p className="text-sm text-white/40">No MCP tools discovered yet.</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {mcpTools.map((tool) => (
              <div
                key={tool.name}
                className="flex items-center justify-between rounded-xl bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04] hover:border-white/[0.08] p-4 transition-colors"
              >
                <div className="flex items-center gap-3">
                  <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-indigo-500/10">
                    <Plug className="h-4 w-4 text-indigo-400" />
                  </div>
                  <div className="min-w-0">
                    <p className="text-sm font-medium text-white">
                      {tool.name}
                    </p>
                    <p className="text-xs text-white/40">
                      from{" "}
                      {typeof tool.source === "object" && "mcp" in tool.source
                        ? tool.source.mcp.name
                        : "unknown"}
                    </p>
                    <p
                      className="truncate text-[11px] text-white/30 max-w-[150px]"
                      title={tool.description}
                    >
                      {tool.description.length > 32
                        ? `${tool.description.slice(0, 32)}...`
                        : tool.description}
                    </p>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default function ModulesPage() {
  const [activeTab, setActiveTab] = useState<TabType>("installed");
  const [mcps, setMcps] = useState<McpServerState[]>([]);
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [selectedMcp, setSelectedMcp] = useState<McpServerState | null>(null);
  const [showAddModal, setShowAddModal] = useState(false);
  const [showConfigureModal, setShowConfigureModal] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [mcpToDelete, setMcpToDelete] = useState<McpServerState | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [toolSearch, setToolSearch] = useState("");

  // Filter MCPs by search
  const filteredMcps = useMemo(() => {
    if (!searchQuery.trim()) return mcps;
    const query = searchQuery.toLowerCase();
    return mcps.filter(
      (m) =>
        m.name.toLowerCase().includes(query) ||
        m.endpoint.toLowerCase().includes(query) ||
        m.tools.some((t) => t.toLowerCase().includes(query))
    );
  }, [mcps, searchQuery]);

  // Filter tools by search
  const filteredTools = useMemo(() => {
    if (!toolSearch.trim()) return tools;
    const query = toolSearch.toLowerCase();
    return tools.filter(
      (t) =>
        t.name.toLowerCase().includes(query) ||
        t.description.toLowerCase().includes(query)
    );
  }, [tools, toolSearch]);

  const fetchData = useCallback(async () => {
    try {
      const [mcpsData, toolsData] = await Promise.all([
        listMcps().catch(() => []),
        listTools().catch(() => []),
      ]);
      setMcps(mcpsData);
      setTools(toolsData);

      // Update selected MCP if it exists
      if (selectedMcp) {
        const updated = mcpsData.find((m) => m.id === selectedMcp.id);
        if (updated) setSelectedMcp(updated);
      }
    } catch (error) {
      console.error("Failed to fetch data:", error);
      toast.error("Failed to fetch modules");
    } finally {
      setLoading(false);
    }
  }, [selectedMcp]);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 5000);
    return () => clearInterval(interval);
  }, [fetchData]);

  const handleAddMcp = async (data: {
    name: string;
    endpoint: string;
    description?: string;
  }) => {
    await addMcp(data);
    toast.success(`Added ${data.name}`);
    await fetchData();
  };

  const handleConfigureMcp = async (data: {
    name: string;
    endpoint: string;
    description?: string;
  }) => {
    if (!selectedMcp) return;
    // For now, we'll remove and re-add since there's no update endpoint
    // In a full implementation, you'd have an updateMcp API endpoint
    await removeMcp(selectedMcp.id);
    const newMcp = await addMcp(data);
    setSelectedMcp(newMcp);
    toast.success(`Updated ${data.name}`);
    await fetchData();
  };

  const handleToggleMcp = async (mcp: McpServerState) => {
    try {
      if (mcp.enabled) {
        await disableMcp(mcp.id);
        toast.success(`Disabled ${mcp.name}`);
      } else {
        await enableMcp(mcp.id);
        toast.success(`Enabled ${mcp.name}`);
      }
      await fetchData();
    } catch (error) {
      console.error("Failed to toggle MCP:", error);
      toast.error(`Failed to toggle ${mcp.name}`);
    }
  };

  const handleRefreshMcp = async (mcp: McpServerState) => {
    try {
      await refreshMcp(mcp.id);
      toast.success(`Refreshed ${mcp.name}`);
      await fetchData();
    } catch (error) {
      console.error("Failed to refresh MCP:", error);
      toast.error(`Failed to refresh ${mcp.name}`);
    }
  };

  const handleDeleteMcp = async (mcp: McpServerState) => {
    setMcpToDelete(mcp);
    setShowDeleteConfirm(true);
  };

  const confirmDeleteMcp = async () => {
    if (!mcpToDelete) return;
    try {
      await removeMcp(mcpToDelete.id);
      toast.success(`Removed ${mcpToDelete.name}`);
      setSelectedMcp(null);
      await fetchData();
    } catch (error) {
      console.error("Failed to delete MCP:", error);
      toast.error(`Failed to remove ${mcpToDelete.name}`);
    } finally {
      setShowDeleteConfirm(false);
      setMcpToDelete(null);
    }
  };

  const handleRefreshAll = async () => {
    setRefreshing(true);
    try {
      await refreshAllMcps();
      toast.success("Refreshed all MCP servers");
      await fetchData();
    } catch (error) {
      console.error("Failed to refresh MCPs:", error);
      toast.error("Failed to refresh MCP servers");
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="h-screen overflow-auto p-6">
      {/* Header */}
      <div className="mb-6 flex items-start justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white">Modules</h1>
          <p className="mt-1 text-sm text-white/50">
            Manage and discover check modules
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleRefreshAll}
            disabled={refreshing}
            className="flex items-center gap-2 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-3 py-2 text-sm text-white/80 transition-colors disabled:opacity-50"
          >
            <RefreshCw
              className={cn("h-4 w-4", refreshing && "animate-spin")}
            />
            Refresh
          </button>
          <button
            onClick={() => setShowAddModal(true)}
            className="flex items-center gap-2 rounded-lg bg-indigo-500 hover:bg-indigo-600 px-3 py-2 text-sm font-medium text-white transition-colors"
          >
            <Plus className="h-4 w-4" />
            Add MCP
          </button>
        </div>
      </div>

      {/* Tabs */}
      <div className="mb-6 flex items-center justify-between flex-wrap gap-4">
        <div className="inline-flex rounded-lg bg-white/[0.02] border border-white/[0.04] p-1">
          <button
            onClick={() => setActiveTab("installed")}
            className={cn(
              "px-4 py-2 rounded-md text-sm font-medium transition-colors",
              activeTab === "installed"
                ? "bg-white/[0.08] text-white"
                : "text-white/40 hover:text-white/60"
            )}
          >
            Installed ({mcps.length})
          </button>
          <button
            onClick={() => setActiveTab("tools")}
            className={cn(
              "px-4 py-2 rounded-md text-sm font-medium transition-colors",
              activeTab === "tools"
                ? "bg-white/[0.08] text-white"
                : "text-white/40 hover:text-white/60"
            )}
          >
            Tools ({tools.length})
          </button>
        </div>

        {/* Search */}
        <div className="relative w-64">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
          <input
            type="text"
            placeholder={
              activeTab === "installed" ? "Search MCPs..." : "Search tools..."
            }
            value={activeTab === "installed" ? searchQuery : toolSearch}
            onChange={(e) =>
              activeTab === "installed"
                ? setSearchQuery(e.target.value)
                : setToolSearch(e.target.value)
            }
            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] py-2 pl-9 pr-3 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
          />
        </div>
      </div>

      {/* Content */}
      {loading ? (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <ShimmerCard />
          <ShimmerCard />
          <ShimmerCard />
        </div>
      ) : activeTab === "installed" ? (
        filteredMcps.length === 0 ? (
          <div className="flex flex-col items-center py-16 text-center">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
              <Plug className="h-8 w-8 text-white/30" />
            </div>
            <p className="text-white/80">
              {searchQuery
                ? "No MCPs match your search"
                : "No MCP servers configured"}
            </p>
            <p className="mt-1 text-sm text-white/40">
              {searchQuery
                ? "Try a different search term"
                : 'Click "Add MCP" to connect to an MCP server'}
            </p>
          </div>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {filteredMcps.map((mcp) => (
              <McpCard
                key={mcp.id}
                mcp={mcp}
                onSelect={setSelectedMcp}
                isSelected={selectedMcp?.id === mcp.id}
              />
            ))}
          </div>
        )
      ) : (
        <ToolsTab tools={filteredTools} />
      )}

      {/* Detail panel (overlay) */}
      {selectedMcp && (
        <McpDetailPanel
          mcp={selectedMcp}
          onClose={() => setSelectedMcp(null)}
          onToggle={() => handleToggleMcp(selectedMcp)}
          onRefresh={() => handleRefreshMcp(selectedMcp)}
          onConfigure={() => setShowConfigureModal(true)}
          onDelete={() => handleDeleteMcp(selectedMcp)}
        />
      )}

      {/* Add modal */}
      {showAddModal && (
        <AddMcpModal
          onClose={() => setShowAddModal(false)}
          onAdd={handleAddMcp}
        />
      )}

      {/* Configure modal */}
      {showConfigureModal && selectedMcp && (
        <ConfigureMcpModal
          mcp={selectedMcp}
          onClose={() => setShowConfigureModal(false)}
          onSave={handleConfigureMcp}
        />
      )}

      {/* Delete confirmation dialog */}
      <ConfirmDialog
        open={showDeleteConfirm}
        title={`Remove ${mcpToDelete?.name}?`}
        description="This will disconnect the MCP server and remove it from your configuration. This action cannot be undone."
        confirmLabel="Remove"
        variant="danger"
        onConfirm={confirmDeleteMcp}
        onCancel={() => {
          setShowDeleteConfirm(false);
          setMcpToDelete(null);
        }}
      />
    </div>
  );
}
