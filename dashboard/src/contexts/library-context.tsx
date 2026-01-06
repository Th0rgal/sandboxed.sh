'use client';

import {
  createContext,
  useContext,
  useCallback,
  useState,
  useEffect,
  useMemo,
  type ReactNode,
} from 'react';
import {
  getLibraryStatus,
  getLibraryMcps,
  listLibrarySkills,
  listLibraryCommands,
  syncLibrary,
  commitLibrary,
  pushLibrary,
  saveLibraryMcps,
  saveLibrarySkill,
  deleteLibrarySkill,
  saveLibraryCommand,
  deleteLibraryCommand,
  LibraryUnavailableError,
  type LibraryStatus,
  type McpServerDef,
  type SkillSummary,
  type CommandSummary,
} from '@/lib/api';

interface LibraryContextValue {
  // State
  status: LibraryStatus | null;
  mcps: Record<string, McpServerDef>;
  skills: SkillSummary[];
  commands: CommandSummary[];
  loading: boolean;
  error: string | null;
  libraryUnavailable: boolean;
  libraryUnavailableMessage: string | null;

  // Actions
  refresh: () => Promise<void>;
  refreshStatus: () => Promise<void>;
  sync: () => Promise<void>;
  commit: (message: string) => Promise<void>;
  push: () => Promise<void>;
  clearError: () => void;

  // MCP operations
  saveMcps: (mcps: Record<string, McpServerDef>) => Promise<void>;

  // Skill operations
  saveSkill: (name: string, content: string) => Promise<void>;
  removeSkill: (name: string) => Promise<void>;

  // Command operations
  saveCommand: (name: string, content: string) => Promise<void>;
  removeCommand: (name: string) => Promise<void>;

  // Operation states
  syncing: boolean;
  committing: boolean;
  pushing: boolean;
}

const LibraryContext = createContext<LibraryContextValue | null>(null);

export function useLibrary() {
  const ctx = useContext(LibraryContext);
  if (!ctx) {
    throw new Error('useLibrary must be used within a LibraryProvider');
  }
  return ctx;
}

interface LibraryProviderProps {
  children: ReactNode;
}

export function LibraryProvider({ children }: LibraryProviderProps) {
  const [status, setStatus] = useState<LibraryStatus | null>(null);
  const [mcps, setMcps] = useState<Record<string, McpServerDef>>({});
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [libraryUnavailable, setLibraryUnavailable] = useState(false);
  const [libraryUnavailableMessage, setLibraryUnavailableMessage] = useState<string | null>(null);

  const [syncing, setSyncing] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [pushing, setPushing] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      setLibraryUnavailable(false);
      setLibraryUnavailableMessage(null);

      const [statusData, mcpsData, skillsData, commandsData] = await Promise.all([
        getLibraryStatus(),
        getLibraryMcps(),
        listLibrarySkills(),
        listLibraryCommands(),
      ]);

      setStatus(statusData);
      setMcps(mcpsData);
      setSkills(skillsData);
      setCommands(commandsData);
    } catch (err) {
      if (err instanceof LibraryUnavailableError) {
        setLibraryUnavailable(true);
        setLibraryUnavailableMessage(err.message);
        setStatus(null);
        setMcps({});
        setSkills([]);
        setCommands([]);
        return;
      }
      setError(err instanceof Error ? err.message : 'Failed to load library data');
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      const statusData = await getLibraryStatus();
      setStatus(statusData);
    } catch (err) {
      // Silently fail status refresh - it's not critical
      console.error('Failed to refresh status:', err);
    }
  }, []);

  // Initial load
  useEffect(() => {
    refresh();
  }, [refresh]);

  const sync = useCallback(async () => {
    try {
      setSyncing(true);
      await syncLibrary();
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to sync');
      throw err;
    } finally {
      setSyncing(false);
    }
  }, [refresh]);

  const commit = useCallback(async (message: string) => {
    try {
      setCommitting(true);
      await commitLibrary(message);
      await refreshStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to commit');
      throw err;
    } finally {
      setCommitting(false);
    }
  }, [refreshStatus]);

  const push = useCallback(async () => {
    try {
      setPushing(true);
      await pushLibrary();
      await refreshStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to push');
      throw err;
    } finally {
      setPushing(false);
    }
  }, [refreshStatus]);

  const saveMcps = useCallback(async (newMcps: Record<string, McpServerDef>) => {
    await saveLibraryMcps(newMcps);
    setMcps(newMcps);
    await refreshStatus();
  }, [refreshStatus]);

  const saveSkill = useCallback(async (name: string, content: string) => {
    await saveLibrarySkill(name, content);
    // Refresh skills list
    const skillsData = await listLibrarySkills();
    setSkills(skillsData);
    await refreshStatus();
  }, [refreshStatus]);

  const removeSkill = useCallback(async (name: string) => {
    await deleteLibrarySkill(name);
    setSkills((prev) => prev.filter((s) => s.name !== name));
    await refreshStatus();
  }, [refreshStatus]);

  const saveCommand = useCallback(async (name: string, content: string) => {
    await saveLibraryCommand(name, content);
    // Refresh commands list
    const commandsData = await listLibraryCommands();
    setCommands(commandsData);
    await refreshStatus();
  }, [refreshStatus]);

  const removeCommand = useCallback(async (name: string) => {
    await deleteLibraryCommand(name);
    setCommands((prev) => prev.filter((c) => c.name !== name));
    await refreshStatus();
  }, [refreshStatus]);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  const value = useMemo<LibraryContextValue>(
    () => ({
      status,
      mcps,
      skills,
      commands,
      loading,
      error,
      libraryUnavailable,
      libraryUnavailableMessage,
      refresh,
      refreshStatus,
      sync,
      commit,
      push,
      clearError,
      saveMcps,
      saveSkill,
      removeSkill,
      saveCommand,
      removeCommand,
      syncing,
      committing,
      pushing,
    }),
    [
      status,
      mcps,
      skills,
      commands,
      loading,
      error,
      libraryUnavailable,
      libraryUnavailableMessage,
      refresh,
      refreshStatus,
      sync,
      commit,
      push,
      clearError,
      saveMcps,
      saveSkill,
      removeSkill,
      saveCommand,
      removeCommand,
      syncing,
      committing,
      pushing,
    ]
  );

  return (
    <LibraryContext.Provider value={value}>
      {children}
    </LibraryContext.Provider>
  );
}
