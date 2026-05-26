'use client';

import { useState } from 'react';
import { X, ChevronDown, ChevronUp, ListPlus, Trash2 } from 'lucide-react';
import { cn } from '@/lib/utils';

export interface QueueItem {
  id: string;
  content: string;
  agent?: string | null;
}

interface QueueStripProps {
  items: QueueItem[];
  onRemove: (id: string) => void;
  onClearAll: () => void;
  className?: string;
}

export function QueueStrip({ items, onRemove, onClearAll, className }: QueueStripProps) {
  const [expanded, setExpanded] = useState(false);

  if (items.length === 0) return null;

  const truncate = (text: string, maxLen: number) => {
    if (text.length <= maxLen) return text;
    return text.slice(0, maxLen - 1) + '…';
  };

  if (!expanded) {
    const head = items[0];
    return (
      <div
        className={cn(
          'group flex items-center gap-2 rounded-lg border border-indigo-500/25 bg-indigo-500/[0.06] px-2.5 py-1.5 text-xs transition-colors',
          'hover:border-indigo-500/35 hover:bg-indigo-500/[0.09] cursor-pointer select-none',
          className,
        )}
        onClick={() => setExpanded(true)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            setExpanded(true);
          }
        }}
        title="Click to expand queued message(s)"
      >
        <ListPlus className="h-3.5 w-3.5 shrink-0 text-indigo-400" />
        <span className="shrink-0 font-medium text-indigo-300">
          Queued
          <span className="ml-1 rounded bg-indigo-500/20 px-1 tabular-nums text-[10px]">
            {items.length}
          </span>
        </span>
        <span className="min-w-0 flex-1 truncate text-white/55">
          {head.agent && <span className="text-emerald-400">@{head.agent} </span>}
          {truncate(head.content, items.length === 1 ? 80 : 60)}
          {items.length > 1 && (
            <span className="text-white/30"> · +{items.length - 1} more</span>
          )}
        </span>
        {items.length === 1 ? (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onRemove(head.id);
            }}
            className="shrink-0 rounded p-0.5 text-white/40 hover:bg-white/10 hover:text-white/80 transition-colors"
            title="Remove from queue"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        ) : null}
        <ChevronDown className="h-3.5 w-3.5 shrink-0 text-white/35 transition-transform group-hover:text-white/60" />
      </div>
    );
  }

  // Expanded view
  return (
    <div
      className={cn(
        'rounded-lg border border-indigo-500/25 bg-indigo-500/[0.06] overflow-hidden',
        className,
      )}
    >
      <div className="flex items-center justify-between gap-2 border-b border-indigo-500/15 px-2.5 py-1.5">
        <div className="flex items-center gap-2 text-xs">
          <ListPlus className="h-3.5 w-3.5 text-indigo-400" />
          <span className="font-medium text-indigo-300">Queued</span>
          <span className="rounded bg-indigo-500/20 px-1 text-[10px] tabular-nums text-indigo-300">
            {items.length}
          </span>
        </div>
        <div className="flex items-center gap-0.5">
          {items.length > 1 && (
            <button
              onClick={onClearAll}
              className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-red-400 hover:bg-red-500/10 transition-colors"
              title="Clear all queued messages"
            >
              <Trash2 className="h-3 w-3" />
              Clear all
            </button>
          )}
          <button
            onClick={() => setExpanded(false)}
            className="rounded p-0.5 text-white/40 hover:bg-white/10 hover:text-white/80 transition-colors"
            title="Collapse"
          >
            <ChevronUp className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      <ul className="max-h-44 divide-y divide-indigo-500/10 overflow-y-auto">
        {items.map((item, index) => (
          <li
            key={item.id}
            className="group flex items-start gap-2 px-2.5 py-1.5 text-xs hover:bg-indigo-500/[0.05]"
          >
            <span className="w-4 shrink-0 pt-0.5 font-mono text-[10px] tabular-nums text-white/30">
              {index + 1}
            </span>
            <p className="min-w-0 flex-1 break-words text-white/75 leading-snug">
              {item.agent && (
                <span className="text-emerald-400">@{item.agent} </span>
              )}
              {item.content}
            </p>
            <button
              onClick={(e) => {
                e.stopPropagation();
                onRemove(item.id);
              }}
              className="shrink-0 rounded p-0.5 text-white/30 opacity-0 transition-opacity hover:bg-white/10 hover:text-red-400 group-hover:opacity-100 focus:opacity-100"
              title="Remove from queue"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
