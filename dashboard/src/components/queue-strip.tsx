'use client';

import { useState } from 'react';
import { X, ChevronDown, ChevronUp, Trash2 } from 'lucide-react';
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
    return text.slice(0, maxLen - 3) + '...';
  };

  // Collapsed view: show first item preview
  if (!expanded && items.length === 1) {
    const item = items[0];
    return (
      <div className={cn(
        "flex items-center gap-2 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20 text-xs",
        className
      )}>
        <span className="text-amber-400 font-medium shrink-0">Queue (1)</span>
        <span className="text-white/60 truncate flex-1">
          {item.agent && <span className="text-emerald-400">@{item.agent} </span>}
          {truncate(item.content, 60)}
        </span>
        <button
          onClick={() => onRemove(item.id)}
          className="p-1 rounded hover:bg-white/10 text-white/40 hover:text-white/70 transition-colors shrink-0"
          title="Remove from queue"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  // Collapsed view with multiple items
  if (!expanded) {
    return (
      <div className={cn(
        "flex items-center gap-2 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20 text-xs",
        className
      )}>
        <span className="text-amber-400 font-medium shrink-0">Queue ({items.length})</span>
        <span className="text-white/50 truncate flex-1">
          {truncate(items[0].content, 40)}
          {items.length > 1 && <span className="text-white/30"> +{items.length - 1} more</span>}
        </span>
        <button
          onClick={() => setExpanded(true)}
          className="p-1 rounded hover:bg-white/10 text-white/40 hover:text-white/70 transition-colors shrink-0"
          title="Expand queue"
        >
          <ChevronDown className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  // Expanded view
  return (
    <div className={cn(
      "rounded-lg bg-amber-500/10 border border-amber-500/20 overflow-hidden",
      className
    )}>
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-amber-500/20">
        <span className="text-amber-400 font-medium text-xs">Queued Messages ({items.length})</span>
        <div className="flex items-center gap-1">
          {items.length > 1 && (
            <button
              onClick={onClearAll}
              className="flex items-center gap-1 px-2 py-1 rounded text-[10px] text-red-400 hover:bg-red-500/10 transition-colors"
              title="Clear all queued messages"
            >
              <Trash2 className="h-3 w-3" />
              Clear All
            </button>
          )}
          <button
            onClick={() => setExpanded(false)}
            className="p-1 rounded hover:bg-white/10 text-white/40 hover:text-white/70 transition-colors"
            title="Collapse"
          >
            <ChevronUp className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      {/* Queue items */}
      <div className="max-h-40 overflow-y-auto">
        {items.map((item, index) => (
          <div
            key={item.id}
            className={cn(
              "flex items-start gap-2 px-3 py-2 text-xs",
              index < items.length - 1 && "border-b border-amber-500/10"
            )}
          >
            <span className="text-white/30 font-mono shrink-0 w-4">{index + 1}.</span>
            <div className="flex-1 min-w-0">
              <p className="text-white/70 break-words">
                {item.agent && <span className="text-emerald-400">@{item.agent} </span>}
                {item.content}
              </p>
            </div>
            <button
              onClick={() => onRemove(item.id)}
              className="p-1 rounded hover:bg-white/10 text-white/40 hover:text-red-400 transition-colors shrink-0"
              title="Remove from queue"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
