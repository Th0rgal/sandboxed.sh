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
import { useToast } from '@/components/toast';
import {
  getLibraryStatus,
  getLibraryMcps,
  listLibrarySkills,
  listLibraryCommands,
  syncLibrary,
  forceSyncLibrary,
  forcePushLibrary,
  commitLibrary,
  pushLibrary,
  saveLibraryMcps,
  saveLibrarySkill,
  deleteLibrarySkill,
  saveLibraryCommand,
  deleteLibraryCommand,
  getLibraryPlugins,
  saveLibraryPlugins,
  listLibraryAgents,
  getLibraryAgent as apiGetLibraryAgent,
  saveLibraryAgent as apiSaveLibraryAgent,
  deleteLibraryAgent,
  LibraryUnavailableError,
  DivergedHistoryError,
  type LibraryStatus,
  type McpServerDef,
  type SkillSummary,
  type CommandSummary,
  type Plugin,
  type LibraryAgentSummary,
  type LibraryAgent,
} from '@/lib/api';

// Re-export types for consumers
export type { LibraryAgentSummary };

interface LibraryContextValue {
  // State
  status: LibraryStatus | null;
  mcps: Record<string, McpServerDef>;
  skills: SkillSummary[];
  commands: CommandSummary[];
  plugins: Record<string, Plugin>;
  libraryAgents: LibraryAgentSummary[];
  loading: boolean;
  libraryUnavailable: boolean;
  libraryUnavailableMessage: string | null;
  /** Set when sync fails due to diverged history (e.g., after force push on remote) */
  divergedHistory: boolean;
  divergedHistoryMessage: string | null;

  // Actions
  refresh: () => Promise<void>;
  refreshStatus: () => Promise<void>;
  sync: () => Promise<void>;
  /** Force reset local to match remote (use after diverged history) */
  forceSync: () => Promise<void>;
  /** Force push local to remote (use when you want to keep local changes) */
  forcePush: () => Promise<void>;
  commit: (message: string) => Promise<void>;
  push: () => Promise<void>;

  // MCP operations
  saveMcps: (mcps: Record<string, McpServerDef>) => Promise<void>;

  // Skill operations
  saveSkill: (name: string, content: string) => Promise<void>;
  removeSkill: (name: string) => Promise<void>;

  // Command operations
  saveCommand: (name: string, content: string) => Promise<void>;
  removeCommand: (name: string) => Promise<void>;

  // Plugin operations
  savePlugins: (plugins: Record<string, Plugin>) => Promise<void>;
  refreshPlugins: () => Promise<void>;

  // Library Agent operations
  getLibraryAgent: (name: string) => Promise<LibraryAgent>;
  saveLibraryAgent: (name: string, content: string) => Promise<void>;
  removeLibraryAgent: (name: string) => Promise<void>;
  refreshLibraryAgents: () => Promise<void>;

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
  const { showError } = useToast();
  const [status, setStatus] = useState<LibraryStatus | null>(null);
  const [mcps, setMcps] = useState<Record<string, McpServerDef>>({});
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [plugins, setPlugins] = useState<Record<string, Plugin>>({});
  const [libraryAgents, setLibraryAgents] = useState<LibraryAgentSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [libraryUnavailable, setLibraryUnavailable] = useState(false);
  const [libraryUnavailableMessage, setLibraryUnavailableMessage] = useState<string | null>(null);

  const [syncing, setSyncing] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [pushing, setPushing] = useState(false);
  const [divergedHistory, setDivergedHistory] = useState(false);
  const [divergedHistoryMessage, setDivergedHistoryMessage] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      setLibraryUnavailable(false);
      setLibraryUnavailableMessage(null);

      const [statusData, mcpsData, skillsData, commandsData, pluginsData, agentsData] = await Promise.all([
        getLibraryStatus(),
        getLibraryMcps(),
        listLibrarySkills(),
        listLibraryCommands(),
        getLibraryPlugins().catch(() => ({})), // May not exist yet
        listLibraryAgents().catch(() => []),
      ]);

      setStatus(statusData);
      setMcps(mcpsData);
      setSkills(skillsData);
      setCommands(commandsData);
      setPlugins(pluginsData);
      setLibraryAgents(agentsData);
    } catch (err) {
      if (err instanceof LibraryUnavailableError) {
        setLibraryUnavailable(true);
        setLibraryUnavailableMessage(err.message);
        setStatus(null);
        setMcps({});
        setSkills([]);
        setCommands([]);
        setPlugins({});
        setLibraryAgents([]);
        return;
      }
      showError(err instanceof Error ? err.message : 'Failed to load library data');
    } finally {
      setLoading(false);
    }
  }, [showError]);

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
      setDivergedHistory(false);
      setDivergedHistoryMessage(null);
      await syncLibrary();
      await refresh();
    } catch (err) {
      if (err instanceof DivergedHistoryError) {
        // Set diverged history state so UI can show force sync options
        setDivergedHistory(true);
        setDivergedHistoryMessage(err.message);
        // Don't show generic error toast - UI will show specific options
        throw err;
      }
      showError(err instanceof Error ? err.message : 'Failed to sync');
      throw err;
    } finally {
      setSyncing(false);
    }
  }, [refresh, showError]);

  const forceSync = useCallback(async () => {
    try {
      setSyncing(true);
      await forceSyncLibrary();
      setDivergedHistory(false);
      setDivergedHistoryMessage(null);
      await refresh();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to force sync');
      throw err;
    } finally {
      setSyncing(false);
    }
  }, [refresh, showError]);

  const forcePush = useCallback(async () => {
    try {
      setPushing(true);
      await forcePushLibrary();
      setDivergedHistory(false);
      setDivergedHistoryMessage(null);
      await refreshStatus();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to force push');
      throw err;
    } finally {
      setPushing(false);
    }
  }, [refreshStatus, showError]);

  const commit = useCallback(async (message: string) => {
    try {
      setCommitting(true);
      await commitLibrary(message);
      await refreshStatus();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to commit');
      throw err;
    } finally {
      setCommitting(false);
    }
  }, [refreshStatus, showError]);

  const push = useCallback(async () => {
    try {
      setPushing(true);
      await pushLibrary();
      await refreshStatus();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to push');
      throw err;
    } finally {
      setPushing(false);
    }
  }, [refreshStatus, showError]);

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

  // Plugin operations
  const _savePlugins = useCallback(async (newPlugins: Record<string, Plugin>) => {
    await saveLibraryPlugins(newPlugins);
    setPlugins(newPlugins);
    await refreshStatus();
  }, [refreshStatus]);

  const refreshPlugins = useCallback(async () => {
    try {
      const pluginsData = await getLibraryPlugins();
      setPlugins(pluginsData);
    } catch {
      // Silently fail - plugins may not exist yet
    }
  }, []);

  // Library Agent operations
  const getLibraryAgent = useCallback(async (name: string): Promise<LibraryAgent> => {
    return apiGetLibraryAgent(name);
  }, []);

  const saveLibraryAgentFn = useCallback(async (name: string, content: string) => {
    // Build a partial LibraryAgent object from content - server handles parsing
    const agent: LibraryAgent = {
      name,
      content,
      description: null,
      path: `agent/${name}.md`,
      model: null,
      tools: {},
      permissions: {},
    };
    await apiSaveLibraryAgent(name, agent);
    const agentsData = await listLibraryAgents();
    setLibraryAgents(agentsData);
    await refreshStatus();
  }, [refreshStatus]);

  const removeLibraryAgent = useCallback(async (name: string) => {
    await deleteLibraryAgent(name);
    setLibraryAgents((prev) => prev.filter((a) => a.name !== name));
    await refreshStatus();
  }, [refreshStatus]);

  const refreshLibraryAgents = useCallback(async () => {
    try {
      const agentsData = await listLibraryAgents();
      setLibraryAgents(agentsData);
    } catch {
      // Silently fail
    }
  }, []);

  const value = useMemo<LibraryContextValue>(
    () => ({
      status,
      mcps,
      skills,
      commands,
      plugins,
      libraryAgents,
      loading,
      libraryUnavailable,
      libraryUnavailableMessage,
      divergedHistory,
      divergedHistoryMessage,
      refresh,
      refreshStatus,
      sync,
      forceSync,
      forcePush,
      commit,
      push,
      saveMcps,
      saveSkill,
      removeSkill,
      saveCommand,
      removeCommand,
      savePlugins: _savePlugins,
      refreshPlugins,
      getLibraryAgent,
      saveLibraryAgent: saveLibraryAgentFn,
      removeLibraryAgent,
      refreshLibraryAgents,
      syncing,
      committing,
      pushing,
    }),
    [
      status,
      mcps,
      skills,
      commands,
      plugins,
      libraryAgents,
      loading,
      libraryUnavailable,
      libraryUnavailableMessage,
      divergedHistory,
      divergedHistoryMessage,
      refresh,
      refreshStatus,
      sync,
      forceSync,
      forcePush,
      commit,
      push,
      saveMcps,
      saveSkill,
      removeSkill,
      saveCommand,
      removeCommand,
      _savePlugins,
      refreshPlugins,
      getLibraryAgent,
      saveLibraryAgentFn,
      removeLibraryAgent,
      refreshLibraryAgents,
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
