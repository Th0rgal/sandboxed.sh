'use client';

/**
 * RemoteNodesPanel — fleet status + operator cordons.
 *
 * Lists configured remote runner nodes with their heartbeat status and lets
 * the operator cordon/uncordon each one. A cordoned node stays listed and
 * probed but is excluded from automatic placement (`remote_node_id: "auto"`)
 * until uncordoned; controllers consulting `get_compute_fleet` see
 * `cordoned: true`. Renders nothing when no remote nodes are configured.
 */

import { useState } from 'react';
import useSWR from 'swr';
import { Network, ShieldOff, ShieldCheck } from 'lucide-react';
import { getRemoteNodes, setNodeCordon } from '@/lib/api';
import { cn } from '@/lib/utils';

export function RemoteNodesPanel() {
  const { data, mutate } = useSWR('remote-nodes', getRemoteNodes, {
    refreshInterval: 15000,
    revalidateOnFocus: false,
  });
  const [busyNode, setBusyNode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const nodes = data?.nodes ?? [];
  if (nodes.length === 0) return null;

  const toggle = async (name: string, cordoned: boolean) => {
    setBusyNode(name);
    setError(null);
    try {
      await setNodeCordon(name, cordoned);
      await mutate();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to update cordon');
    } finally {
      setBusyNode(null);
    }
  };

  return (
    <div className="rounded-lg bg-white/[0.02] border border-white/[0.05] p-3">
      <p className="text-xs text-white/60 font-medium flex items-center gap-1.5">
        <Network className="h-3.5 w-3.5 text-white/40" />
        Remote Nodes
      </p>
      <p className="text-[10px] text-white/30 mt-0.5">
        Cordoned nodes stay listed but are excluded from automatic placement.
      </p>

      <div className="mt-2 space-y-1.5">
        {nodes.map((node) => (
          <div
            key={node.id}
            className="flex items-center justify-between gap-2 rounded-lg bg-black/20 border border-white/[0.04] px-2.5 py-1.5"
          >
            <div className="flex items-center gap-2 min-w-0">
              <span
                className={cn(
                  'h-1.5 w-1.5 rounded-full shrink-0',
                  node.status === 'online'
                    ? 'bg-emerald-400/80'
                    : node.status === 'degraded'
                    ? 'bg-amber-400/80'
                    : 'bg-white/20'
                )}
              />
              <span className="text-xs font-mono text-white/80 truncate">{node.id}</span>
              {node.cordoned && (
                <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium bg-amber-500/10 text-amber-400 border border-amber-500/20">
                  cordoned
                </span>
              )}
              {node.labels.length > 0 && (
                <span className="text-[10px] text-white/30 truncate max-sm:hidden">
                  {node.labels.join(', ')}
                </span>
              )}
            </div>
            <button
              onClick={() => toggle(node.id, !node.cordoned)}
              disabled={busyNode === node.id}
              className={cn(
                'shrink-0 flex items-center gap-1 rounded-lg px-2 py-1 text-[10px] font-medium transition-colors',
                busyNode === node.id
                  ? 'bg-white/[0.04] border border-white/[0.06] text-white/30 cursor-wait'
                  : node.cordoned
                  ? 'bg-indigo-500/15 border border-indigo-500/25 text-indigo-200 hover:bg-indigo-500/25'
                  : 'bg-amber-500/10 border border-amber-500/20 text-amber-300 hover:bg-amber-500/20'
              )}
              title={
                node.cordoned
                  ? 'Allow automatic placement on this node again'
                  : 'Exclude this node from automatic placement'
              }
            >
              {node.cordoned ? (
                <ShieldCheck className="h-3 w-3" />
              ) : (
                <ShieldOff className="h-3 w-3" />
              )}
              {busyNode === node.id ? 'Updating…' : node.cordoned ? 'Uncordon' : 'Cordon'}
            </button>
          </div>
        ))}
      </div>

      {error && <p className="text-[10px] text-red-300/80 mt-1.5">{error}</p>}
    </div>
  );
}
