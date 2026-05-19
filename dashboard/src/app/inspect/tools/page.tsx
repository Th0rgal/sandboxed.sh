'use client';

import { useMemo } from 'react';
import useSWR from 'swr';
import { Wrench } from 'lucide-react';
import { listTools, type ToolInfo } from '@/lib/api';
import { cn } from '@/lib/utils';

function formatToolSource(source: ToolInfo['source']): string {
  if (source === 'builtin') return 'Built-in';
  if (typeof source === 'object' && source && 'mcp' in source) {
    const name = source.mcp.name || source.mcp.id;
    return `MCP: ${name}`;
  }
  if (typeof source === 'object' && source && 'plugin' in source) {
    const name = source.plugin.name || source.plugin.id;
    return `Plugin: ${name}`;
  }
  return 'Unknown';
}

export default function ToolsPage() {
  // SWR: fetch tools list
  const { data: tools = [], isLoading: loading } = useSWR(
    'tools',
    listTools,
    { revalidateOnFocus: false }
  );

  const sortedTools = useMemo(() => {
    return [...tools].sort((a, b) => a.name.localeCompare(b.name));
  }, [tools]);

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-6xl mx-auto space-y-4">
      <div>
        <h1 className="text-2xl font-semibold text-white">Tools</h1>
        <p className="text-sm text-white/60 mt-1">
          Read-only inventory of tools available to agents, including their source.
        </p>
      </div>

      <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
        <div className="grid grid-cols-[minmax(180px,1.1fr)_minmax(160px,0.8fr)_minmax(280px,2fr)_minmax(110px,0.6fr)] gap-4 px-4 py-3 text-[11px] uppercase tracking-wider text-white/40 border-b border-white/[0.06]">
          <span>Name</span>
          <span>Source</span>
          <span>Description</span>
          <span>Status</span>
        </div>

        {loading ? (
          <div className="divide-y divide-white/[0.04]">
            {Array.from({ length: 10 }).map((_, index) => (
              <div
                key={index}
                className="grid grid-cols-[minmax(180px,1.1fr)_minmax(160px,0.8fr)_minmax(280px,2fr)_minmax(110px,0.6fr)] gap-4 px-4 py-3"
              >
                <div className="h-4 w-32 rounded bg-white/[0.06]" />
                <div className="h-4 w-24 rounded bg-white/[0.04]" />
                <div className="h-4 w-full rounded bg-white/[0.04]" />
                <div className="h-4 w-16 rounded bg-white/[0.04]" />
              </div>
            ))}
          </div>
        ) : sortedTools.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16 text-white/40">
            <Wrench className="h-10 w-10 mb-3 text-white/20" />
            <p className="text-sm">No tools available</p>
          </div>
        ) : (
          <div className="divide-y divide-white/[0.04]">
            {sortedTools.map((tool) => {
              const sourceLabel = formatToolSource(tool.source);
              return (
                <div
                  key={tool.name}
                  className="grid grid-cols-[minmax(180px,1.1fr)_minmax(160px,0.8fr)_minmax(280px,2fr)_minmax(110px,0.6fr)] gap-4 px-4 py-3 text-sm"
                >
                  <div className="font-medium text-white truncate">{tool.name}</div>
                  <div className="text-xs text-white/60 truncate">{sourceLabel}</div>
                  <div className="text-xs text-white/50 line-clamp-2">{tool.description}</div>
                  <div
                    className={cn(
                      'text-xs font-medium',
                      tool.enabled ? 'text-emerald-400' : 'text-white/40'
                    )}
                  >
                    {tool.enabled ? 'Enabled' : 'Disabled'}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
