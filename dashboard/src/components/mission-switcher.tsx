'use client';

import { useEffect, useRef, useState, useMemo } from 'react';
import { Search, XCircle, Check } from 'lucide-react';
import { cn } from '@/lib/utils';
import { type Mission, type MissionStatus, type RunningMissionInfo } from '@/lib/api';

interface MissionSwitcherProps {
  open: boolean;
  onClose: () => void;
  missions: Mission[];
  runningMissions: RunningMissionInfo[];
  currentMissionId?: string | null;
  viewingMissionId?: string | null;
  onSelectMission: (missionId: string) => void;
  onCancelMission: (missionId: string) => void;
  onRefresh?: () => void;
}

function missionStatusDotClass(status: MissionStatus): string {
  switch (status) {
    case 'active':
      return 'bg-emerald-400';
    case 'completed':
      return 'bg-emerald-400';
    case 'failed':
      return 'bg-red-400';
    case 'interrupted':
      return 'bg-amber-400';
    case 'blocked':
      return 'bg-orange-400';
    case 'not_feasible':
      return 'bg-rose-400';
  }
}

function missionStatusLabel(status: MissionStatus): string {
  switch (status) {
    case 'active':
      return 'Active';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'interrupted':
      return 'Interrupted';
    case 'blocked':
      return 'Blocked';
    case 'not_feasible':
      return 'Not Feasible';
  }
}

function getMissionDisplayName(mission: Mission): string {
  return mission.workspace_name || mission.id.slice(0, 8);
}

function getMissionDescription(mission: Mission): string {
  if (mission.title) return mission.title;
  // Get first user message from history as fallback
  const firstUserMessage = mission.history?.find(h => h.role === 'user');
  if (firstUserMessage?.content) {
    const content = firstUserMessage.content.trim();
    return content.length > 60 ? content.slice(0, 60) + '...' : content;
  }
  return '';
}

export function MissionSwitcher({
  open,
  onClose,
  missions,
  runningMissions,
  currentMissionId,
  viewingMissionId,
  onSelectMission,
  onCancelMission,
  onRefresh,
}: MissionSwitcherProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);

  // Compute filtered missions
  const runningMissionIds = useMemo(
    () => new Set(runningMissions.map((m) => m.mission_id)),
    [runningMissions]
  );

  const recentMissions = useMemo(() => {
    return missions.filter(
      (m) => m.id !== currentMissionId && !runningMissionIds.has(m.id)
    );
  }, [missions, currentMissionId, runningMissionIds]);

  // Build flat list of all selectable items
  const allItems = useMemo(() => {
    const items: Array<{
      type: 'running' | 'current' | 'recent';
      mission?: Mission;
      runningInfo?: RunningMissionInfo;
      id: string;
    }> = [];

    // Current mission first if not running
    if (currentMissionId) {
      const currentMission = missions.find((m) => m.id === currentMissionId);
      if (currentMission && !runningMissionIds.has(currentMissionId)) {
        items.push({ type: 'current', mission: currentMission, id: currentMissionId });
      }
    }

    // Running missions
    runningMissions.forEach((rm) => {
      const mission = missions.find((m) => m.id === rm.mission_id);
      items.push({
        type: 'running',
        mission,
        runningInfo: rm,
        id: rm.mission_id,
      });
    });

    // Recent missions
    recentMissions.forEach((m) => {
      items.push({ type: 'recent', mission: m, id: m.id });
    });

    return items;
  }, [missions, currentMissionId, runningMissions, runningMissionIds, recentMissions]);

  // Filter items by search query
  const filteredItems = useMemo(() => {
    if (!searchQuery.trim()) return allItems;
    const query = searchQuery.toLowerCase();
    return allItems.filter((item) => {
      if (!item.mission) return false;
      const name = getMissionDisplayName(item.mission).toLowerCase();
      const desc = getMissionDescription(item.mission).toLowerCase();
      return name.includes(query) || desc.includes(query);
    });
  }, [allItems, searchQuery]);

  // Reset state on open/close
  useEffect(() => {
    if (open) {
      setSearchQuery('');
      setSelectedIndex(0);
      // Focus input after animation
      setTimeout(() => inputRef.current?.focus(), 50);
      // Refresh missions list
      onRefresh?.();
    }
  }, [open, onRefresh]);

  // Reset selected index when filter changes
  useEffect(() => {
    setSelectedIndex(0);
  }, [searchQuery]);

  // Handle keyboard navigation
  useEffect(() => {
    if (!open) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      switch (e.key) {
        case 'Escape':
          e.preventDefault();
          onClose();
          break;
        case 'ArrowDown':
          e.preventDefault();
          setSelectedIndex((prev) =>
            Math.min(prev + 1, filteredItems.length - 1)
          );
          break;
        case 'ArrowUp':
          e.preventDefault();
          setSelectedIndex((prev) => Math.max(prev - 1, 0));
          break;
        case 'Enter':
          e.preventDefault();
          if (filteredItems[selectedIndex]) {
            onSelectMission(filteredItems[selectedIndex].id);
            onClose();
          }
          break;
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose, filteredItems, selectedIndex, onSelectMission]);

  // Scroll selected item into view
  useEffect(() => {
    if (!listRef.current) return;
    const selectedEl = listRef.current.querySelector('[data-selected="true"]');
    if (selectedEl) {
      selectedEl.scrollIntoView({ block: 'nearest' });
    }
  }, [selectedIndex]);

  // Handle click outside
  useEffect(() => {
    if (!open) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [open, onClose]);

  if (!open) return null;

  const hasRunning = runningMissions.length > 0;
  const hasRecent = recentMissions.length > 0;
  const hasCurrent =
    currentMissionId && !runningMissionIds.has(currentMissionId);

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm animate-in fade-in duration-150" />

      {/* Dialog */}
      <div
        ref={dialogRef}
        className="relative w-full max-w-xl rounded-xl bg-[#1a1a1a] border border-white/[0.06] shadow-2xl animate-in fade-in zoom-in-95 duration-150"
      >
        {/* Search input */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-white/[0.06]">
          <Search className="h-4 w-4 text-white/40 shrink-0" />
          <input
            ref={inputRef}
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search missions..."
            className="flex-1 bg-transparent text-sm text-white placeholder:text-white/40 focus:outline-none"
          />
          <div className="flex items-center gap-1 text-[10px] text-white/30">
            <kbd className="px-1.5 py-0.5 rounded bg-white/[0.06] font-mono">
              esc
            </kbd>
            <span>to close</span>
          </div>
        </div>

        {/* Mission list */}
        <div ref={listRef} className="max-h-[400px] overflow-y-auto py-2">
          {filteredItems.length === 0 ? (
            <div className="px-4 py-8 text-center text-sm text-white/40">
              No missions found
            </div>
          ) : (
            <>
              {/* Current mission */}
              {hasCurrent && !searchQuery && (
                <div className="px-3 pt-1 pb-2">
                  <span className="text-[10px] font-medium uppercase tracking-wider text-white/30">
                    Current
                  </span>
                </div>
              )}
              {filteredItems.map((item, index) => {
                // Show section headers only when not searching
                const showRunningHeader =
                  !searchQuery &&
                  item.type === 'running' &&
                  (index === 0 ||
                    (index === 1 && hasCurrent) ||
                    filteredItems[index - 1]?.type !== 'running');
                const showRecentHeader =
                  !searchQuery &&
                  item.type === 'recent' &&
                  filteredItems[index - 1]?.type !== 'recent';

                const mission = item.mission;
                const isSelected = index === selectedIndex;
                const isViewing = item.id === viewingMissionId;
                const isRunning = item.type === 'running';
                const runningInfo = item.runningInfo;

                const isStalled =
                  isRunning &&
                  runningInfo?.state === 'running' &&
                  (runningInfo?.seconds_since_activity ?? 0) > 60;
                const isSeverlyStalled =
                  isRunning &&
                  runningInfo?.state === 'running' &&
                  (runningInfo?.seconds_since_activity ?? 0) > 120;

                return (
                  <div key={item.id}>
                    {showRunningHeader && (
                      <div className="px-3 pt-3 pb-2 border-t border-white/[0.06] mt-1">
                        <span className="text-[10px] font-medium uppercase tracking-wider text-white/30">
                          Running
                        </span>
                      </div>
                    )}
                    {showRecentHeader && (
                      <div className="px-3 pt-3 pb-2 border-t border-white/[0.06] mt-1">
                        <span className="text-[10px] font-medium uppercase tracking-wider text-white/30">
                          Recent
                        </span>
                      </div>
                    )}
                    <div
                      data-selected={isSelected}
                      onClick={() => {
                        onSelectMission(item.id);
                        onClose();
                      }}
                      className={cn(
                        'group flex items-center gap-3 px-3 py-2 mx-2 rounded-lg cursor-pointer transition-colors',
                        isSelected
                          ? 'bg-indigo-500/15 text-white'
                          : 'text-white/70 hover:bg-white/[0.04]',
                        isSeverlyStalled && 'bg-red-500/10',
                        isStalled && !isSeverlyStalled && 'bg-amber-500/10'
                      )}
                    >
                      {/* Status dot */}
                      <div
                        className={cn(
                          'h-2 w-2 rounded-full shrink-0',
                          mission
                            ? missionStatusDotClass(mission.status)
                            : isRunning
                            ? 'bg-emerald-400'
                            : 'bg-gray-400',
                          isRunning &&
                            runningInfo?.state === 'running' &&
                            'animate-pulse'
                        )}
                      />

                      {/* Mission info */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="font-medium text-sm truncate">
                            {mission
                              ? getMissionDisplayName(mission)
                              : item.id.slice(0, 8)}
                          </span>
                          {isStalled && (
                            <span className="text-[10px] text-amber-400 tabular-nums shrink-0">
                              {Math.floor(runningInfo?.seconds_since_activity ?? 0)}s
                            </span>
                          )}
                        </div>
                        {mission && getMissionDescription(mission) && (
                          <p className="text-xs text-white/40 truncate mt-0.5">
                            {getMissionDescription(mission)}
                          </p>
                        )}
                      </div>

                      {/* Status label */}
                      <span className="text-[10px] text-white/30 shrink-0">
                        {isRunning
                          ? runningInfo?.state || 'running'
                          : mission
                          ? missionStatusLabel(mission.status)
                          : ''}
                      </span>

                      {/* Viewing indicator */}
                      {isViewing && (
                        <Check className="h-4 w-4 text-indigo-400 shrink-0" />
                      )}

                      {/* Cancel button for running missions */}
                      {isRunning && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            onCancelMission(item.id);
                          }}
                          className="p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-white/[0.08] text-white/30 hover:text-red-400 transition-all shrink-0"
                          title="Cancel mission"
                        >
                          <XCircle className="h-4 w-4" />
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
            </>
          )}
        </div>

        {/* Footer hints */}
        <div className="flex items-center justify-between px-4 py-2 border-t border-white/[0.06] text-[10px] text-white/30">
          <div className="flex items-center gap-3">
            <span className="flex items-center gap-1">
              <kbd className="px-1 py-0.5 rounded bg-white/[0.06] font-mono">
                ↑↓
              </kbd>
              navigate
            </span>
            <span className="flex items-center gap-1">
              <kbd className="px-1 py-0.5 rounded bg-white/[0.06] font-mono">
                ↵
              </kbd>
              select
            </span>
          </div>
          <span className="flex items-center gap-1">
            <kbd className="px-1 py-0.5 rounded bg-white/[0.06] font-mono">
              ⌘K
            </kbd>
            to open
          </span>
        </div>
      </div>
    </div>
  );
}
