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
  getLibraryPlugins,
  saveLibraryPlugins,
  listLibraryRules,
  getLibraryRule,
  saveLibraryRule,
  deleteLibraryRule,
  listLibraryAgents,
  getLibraryAgent as apiGetLibraryAgent,
  saveLibraryAgent as apiSaveLibraryAgent,
  deleteLibraryAgent,
  listLibraryTools,
  getLibraryTool as apiGetLibraryTool,
  saveLibraryTool as apiSaveLibraryTool,
  deleteLibraryTool,
  LibraryUnavailableError,
  type LibraryStatus,
  type McpServerDef,
  type SkillSummary,
  type CommandSummary,
  type Plugin,
  type RuleSummary,
  type Rule,
  type LibraryAgentSummary,
  type LibraryAgent,
  type LibraryToolSummary,
  type LibraryTool,
} from '@/lib/api';

interface LibraryContextValue {
  // State
  status: LibraryStatus | null;
  mcps: Record<string, McpServerDef>;
  skills: SkillSummary[];
  commands: CommandSummary[];
  plugins: Record<string, Plugin>;
  rules: RuleSummary[];
  libraryAgents: LibraryAgentSummary[];
  libraryTools: LibraryToolSummary[];
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

  // Plugin operations
  savePlugins: (plugins: Record<string, Plugin>) => Promise<void>;
  refreshPlugins: () => Promise<void>;

  // Rule operations
  getRule: (name: string) => Promise<Rule>;
  saveRule: (name: string, content: string) => Promise<void>;
  removeRule: (name: string) => Promise<void>;
  refreshRules: () => Promise<void>;

  // Library Agent operations
  getLibraryAgent: (name: string) => Promise<LibraryAgent>;
  saveLibraryAgent: (name: string, content: string) => Promise<void>;
  removeLibraryAgent: (name: string) => Promise<void>;
  refreshLibraryAgents: () => Promise<void>;

  // Library Tool operations
  getLibraryTool: (name: string) => Promise<LibraryTool>;
  saveLibraryTool: (name: string, content: string) => Promise<void>;
  removeLibraryTool: (name: string) => Promise<void>;
  refreshLibraryTools: () => Promise<void>;

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
  const [plugins, setPlugins] = useState<Record<string, Plugin>>({});
  const [rules, setRules] = useState<RuleSummary[]>([]);
  const [libraryAgents, setLibraryAgents] = useState<LibraryAgentSummary[]>([]);
  const [libraryTools, setLibraryTools] = useState<LibraryToolSummary[]>([]);
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

      const [statusData, mcpsData, skillsData, commandsData, pluginsData, rulesData, agentsData, toolsData] = await Promise.all([
        getLibraryStatus(),
        getLibraryMcps(),
        listLibrarySkills(),
        listLibraryCommands(),
        getLibraryPlugins().catch(() => ({})), // May not exist yet
        listLibraryRules().catch(() => []),
        listLibraryAgents().catch(() => []),
        listLibraryTools().catch(() => []),
      ]);

      setStatus(statusData);
      setMcps(mcpsData);
      setSkills(skillsData);
      setCommands(commandsData);
      setPlugins(pluginsData);
      setRules(rulesData);
      setLibraryAgents(agentsData);
      setLibraryTools(toolsData);
    } catch (err) {
      if (err instanceof LibraryUnavailableError) {
        setLibraryUnavailable(true);
        setLibraryUnavailableMessage(err.message);
        setStatus(null);
        setMcps({});
        setSkills([]);
        setCommands([]);
        setPlugins({});
        setRules([]);
        setLibraryAgents([]);
        setLibraryTools([]);
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

  // Rule operations
  const saveRule = useCallback(async (name: string, content: string) => {
    await saveLibraryRule(name, content);
    const rulesData = await listLibraryRules();
    setRules(rulesData);
    await refreshStatus();
  }, [refreshStatus]);

  const removeRule = useCallback(async (name: string) => {
    await deleteLibraryRule(name);
    setRules((prev) => prev.filter((r) => r.name !== name));
    await refreshStatus();
  }, [refreshStatus]);

  const refreshRules = useCallback(async () => {
    try {
      const rulesData = await listLibraryRules();
      setRules(rulesData);
    } catch {
      // Silently fail
    }
  }, []);

  // Rule getter
  const getRule = useCallback(async (name: string): Promise<Rule> => {
    return getLibraryRule(name);
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
      skills: [],
      rules: [],
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

  // Library Tool operations
  const getLibraryTool = useCallback(async (name: string): Promise<LibraryTool> => {
    return apiGetLibraryTool(name);
  }, []);

  const saveLibraryToolFn = useCallback(async (name: string, content: string) => {
    await apiSaveLibraryTool(name, content);
    const toolsData = await listLibraryTools();
    setLibraryTools(toolsData);
    await refreshStatus();
  }, [refreshStatus]);

  const removeLibraryTool = useCallback(async (name: string) => {
    await deleteLibraryTool(name);
    setLibraryTools((prev) => prev.filter((t) => t.name !== name));
    await refreshStatus();
  }, [refreshStatus]);

  const refreshLibraryTools = useCallback(async () => {
    try {
      const toolsData = await listLibraryTools();
      setLibraryTools(toolsData);
    } catch {
      // Silently fail
    }
  }, []);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  const value = useMemo<LibraryContextValue>(
    () => ({
      status,
      mcps,
      skills,
      commands,
      plugins,
      rules,
      libraryAgents,
      libraryTools,
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
      savePlugins: _savePlugins,
      refreshPlugins,
      getRule,
      saveRule,
      removeRule,
      refreshRules,
      getLibraryAgent,
      saveLibraryAgent: saveLibraryAgentFn,
      removeLibraryAgent,
      refreshLibraryAgents,
      getLibraryTool,
      saveLibraryTool: saveLibraryToolFn,
      removeLibraryTool,
      refreshLibraryTools,
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
      rules,
      libraryAgents,
      libraryTools,
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
      _savePlugins,
      refreshPlugins,
      getRule,
      saveRule,
      removeRule,
      refreshRules,
      getLibraryAgent,
      saveLibraryAgentFn,
      removeLibraryAgent,
      refreshLibraryAgents,
      getLibraryTool,
      saveLibraryToolFn,
      removeLibraryTool,
      refreshLibraryTools,
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
