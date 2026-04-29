'use client';

import { createContext, useContext, useState, useEffect, useCallback, useMemo } from 'react';
import { useRouter, usePathname } from 'next/navigation';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import { MissionSwitcher } from '@/components/mission-switcher';
import {
  listMissions,
  getRunningMissions,
  loadMission,
  cancelMission,
  resumeMission,
  createMission,
  getMission,
  type Mission,
  type RunningMissionInfo,
} from '@/lib/api';

interface MissionSwitcherContextValue {
  open: () => void;
  close: () => void;
  isOpen: boolean;
}

const MissionSwitcherContext = createContext<MissionSwitcherContextValue | null>(null);

export function useMissionSwitcher() {
  const ctx = useContext(MissionSwitcherContext);
  if (!ctx) {
    throw new Error('useMissionSwitcher must be used within MissionSwitcherProvider');
  }
  return ctx;
}

export function MissionSwitcherProvider({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();
  const [isOpen, setIsOpen] = useState(false);

  // Control page has its own mission switcher with more context (currentMissionId, viewingMissionId)
  const isControlPage = pathname === '/control';

  // SWR: fetch missions
  const { data: missions = [], mutate: mutateMissions } = useSWR<Mission[]>(
    'global-missions',
    listMissions,
    {
      refreshInterval: 5000,
      revalidateOnFocus: false,
    }
  );

  // SWR: fetch running missions
  const { data: runningMissions = [], mutate: mutateRunningMissions } = useSWR<RunningMissionInfo[]>(
    'global-running-missions',
    getRunningMissions,
    {
      refreshInterval: 3000,
      revalidateOnFocus: false,
    }
  );

  // Global keyboard shortcut for Cmd+K / Ctrl+K
  // Skip on control page which has its own handler
  useEffect(() => {
    if (isControlPage) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        setIsOpen(true);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isControlPage]);

  const handleSelectMission = useCallback(async (missionId: string) => {
    const mission = await loadMission(missionId);
    if (!mission) {
      toast.error('Mission not found');
      return;
    }
    router.push(`/control?mission=${missionId}`);
  }, [router]);

  const handleCancelMission = useCallback(async (missionId: string) => {
    try {
      await cancelMission(missionId);
      toast.success('Mission cancelled');
      await Promise.all([mutateMissions(), mutateRunningMissions()]);
    } catch {
      toast.error('Failed to cancel mission');
    }
  }, [mutateMissions, mutateRunningMissions]);

  const handleRefresh = useCallback(() => {
    mutateMissions();
  }, [mutateMissions]);

  const handleResumeMission = useCallback(async (missionId: string) => {
    try {
      await resumeMission(missionId);
      await Promise.all([mutateMissions(), mutateRunningMissions()]);
      toast.success('Mission resumed');
      router.push(`/control?mission=${missionId}`);
    } catch {
      toast.error('Failed to resume mission');
    }
  }, [mutateMissions, mutateRunningMissions, router]);

  const handleOpenFailingToolCall = useCallback(async (missionId: string) => {
    router.push(`/control?mission=${missionId}&focus=failure`);
  }, [router]);

  const handleFollowUpMission = useCallback(async (missionId: string) => {
    try {
      const sourceMission = missions.find((mission) => mission.id === missionId) ?? (await getMission(missionId));
      if (!sourceMission) {
        toast.error('Source mission not found');
        return;
      }

      const followUpMission = await createMission({
        workspaceId: sourceMission.workspace_id,
        agent: sourceMission.agent || undefined,
        modelOverride: sourceMission.model_override || undefined,
        modelEffort: sourceMission.model_effort || undefined,
        backend: sourceMission.backend,
      });
      await Promise.all([mutateMissions(), mutateRunningMissions()]);
      toast.success('Follow-up mission created');
      router.push(`/control?mission=${followUpMission.id}`);
    } catch {
      toast.error('Failed to create follow-up mission');
    }
  }, [missions, mutateMissions, mutateRunningMissions, router]);

  const contextValue = useMemo(() => ({
    open: () => setIsOpen(true),
    close: () => setIsOpen(false),
    isOpen,
  }), [isOpen]);

  return (
    <MissionSwitcherContext.Provider value={contextValue}>
      {children}
      {/* Don't render on control page - it has its own mission switcher with more context */}
      {!isControlPage && (
        <MissionSwitcher
          open={isOpen}
          onClose={() => setIsOpen(false)}
          missions={missions}
          runningMissions={runningMissions}
          currentMissionId={null}
          viewingMissionId={null}
          onSelectMission={handleSelectMission}
          onCancelMission={handleCancelMission}
          onResumeMission={handleResumeMission}
          onOpenFailingToolCall={handleOpenFailingToolCall}
          onFollowUpMission={handleFollowUpMission}
          onRefresh={handleRefresh}
        />
      )}
    </MissionSwitcherContext.Provider>
  );
}
