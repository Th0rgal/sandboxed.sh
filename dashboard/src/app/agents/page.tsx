'use client';

import { useEffect, useState, useRef } from 'react';
import {
  listAgents,
  getAgent,
  createAgent,
  updateAgent,
  deleteAgent,
  listProviders,
  listLibrarySkills,
  listLibraryCommands,
  listMcps,
  type AgentConfig,
  type Provider,
  type SkillSummary,
  type CommandSummary,
  type McpServerState,
} from '@/lib/api';
import {
  Plus,
  Save,
  Trash2,
  X,
  Loader,
  AlertCircle,
  Cpu,
  Settings,
  FileText,
  Terminal,
} from 'lucide-react';
import { cn } from '@/lib/utils';

export default function AgentsPage() {
  const [agents, setAgents] = useState<AgentConfig[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<AgentConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [providers, setProviders] = useState<Provider[]>([]);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServerState[]>([]);

  const [showNewAgentDialog, setShowNewAgentDialog] = useState(false);
  const [newAgentName, setNewAgentName] = useState('');
  const [newAgentModel, setNewAgentModel] = useState('');

  const [dirty, setDirty] = useState(false);
  const [editedAgent, setEditedAgent] = useState<AgentConfig | null>(null);

  // Ref to track agent state for dirty flag comparison
  const editedAgentRef = useRef(editedAgent);
  editedAgentRef.current = editedAgent;

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [agentsData, providersData, skillsData, commandsData, mcpData] = await Promise.all([
        listAgents(),
        listProviders(),
        listLibrarySkills().catch(() => []),
        listLibraryCommands().catch(() => []),
        listMcps().catch(() => []),
      ]);
      setAgents(agentsData);
      setProviders(providersData.providers);
      setSkills(skillsData);
      setCommands(commandsData);
      setMcpServers(mcpData);

      // Set default model if available
      if (providersData.providers.length > 0 && providersData.providers[0].models.length > 0) {
        setNewAgentModel(providersData.providers[0].models[0].id);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load data');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const loadAgent = async (id: string) => {
    try {
      const agent = await getAgent(id);
      setSelectedAgent(agent);
      setEditedAgent(agent);
      setDirty(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load agent');
    }
  };

  const handleCreateAgent = async () => {
    if (!newAgentName.trim() || !newAgentModel.trim()) return;
    try {
      setSaving(true);
      const agent = await createAgent({
        name: newAgentName,
        model_id: newAgentModel,
      });
      await loadData();
      setShowNewAgentDialog(false);
      setNewAgentName('');
      await loadAgent(agent.id);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create agent');
    } finally {
      setSaving(false);
    }
  };

  const handleSaveAgent = async () => {
    if (!editedAgent || !selectedAgent) return;
    // Capture the state being saved for comparison after save completes
    const agentBeingSaved = { ...editedAgent };
    try {
      setSaving(true);
      await updateAgent(editedAgent.id, {
        name: editedAgent.name,
        model_id: editedAgent.model_id,
        mcp_servers: editedAgent.mcp_servers,
        skills: editedAgent.skills,
        commands: editedAgent.commands,
      });
      await loadData();
      // Only clear dirty and reload if state hasn't changed during save
      const currentAgent = editedAgentRef.current;
      if (currentAgent && JSON.stringify(currentAgent) === JSON.stringify(agentBeingSaved)) {
        // State unchanged - safe to reload and clear dirty
        await loadAgent(agentBeingSaved.id);
        setDirty(false);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save agent');
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteAgent = async () => {
    if (!selectedAgent) return;
    if (!confirm(`Delete agent "${selectedAgent.name}"?`)) return;
    try {
      await deleteAgent(selectedAgent.id);
      setSelectedAgent(null);
      setEditedAgent(null);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete agent');
    }
  };

  const toggleMcpServer = (serverName: string) => {
    if (!editedAgent) return;
    const newServers = editedAgent.mcp_servers.includes(serverName)
      ? editedAgent.mcp_servers.filter((s) => s !== serverName)
      : [...editedAgent.mcp_servers, serverName];
    setEditedAgent({ ...editedAgent, mcp_servers: newServers });
    setDirty(true);
  };

  const toggleSkill = (skillName: string) => {
    if (!editedAgent) return;
    const newSkills = editedAgent.skills.includes(skillName)
      ? editedAgent.skills.filter((s) => s !== skillName)
      : [...editedAgent.skills, skillName];
    setEditedAgent({ ...editedAgent, skills: newSkills });
    setDirty(true);
  };

  const toggleCommand = (commandName: string) => {
    if (!editedAgent) return;
    const newCommands = editedAgent.commands.includes(commandName)
      ? editedAgent.commands.filter((c) => c !== commandName)
      : [...editedAgent.commands, commandName];
    setEditedAgent({ ...editedAgent, commands: newCommands });
    setDirty(true);
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-7xl mx-auto space-y-4">
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
          <button onClick={() => setError(null)} className="ml-auto">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold text-white">Agents</h1>
          <p className="text-sm text-white/60 mt-1">
            Configure agent models, skills, commands, and MCP servers
          </p>
        </div>
        <button
          onClick={() => setShowNewAgentDialog(true)}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
        >
          <Plus className="h-4 w-4" />
          New Agent
        </button>
      </div>

      <div className="flex-1 min-h-0 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
        <div className="flex flex-1 min-h-0 items-stretch">
          {/* Agent List */}
          <div className="w-64 border-r border-white/[0.06] flex flex-col min-h-0">
            <div className="p-3 border-b border-white/[0.06]">
              <span className="text-xs font-medium text-white/60">
                Agents{agents.length ? ` (${agents.length})` : ''}
              </span>
            </div>
            <div className="flex-1 min-h-0 overflow-y-auto p-2">
              {agents.length === 0 ? (
                <p className="text-xs text-white/40 text-center py-4">No agents yet</p>
              ) : (
                agents.map((agent) => (
                  <button
                    key={agent.id}
                    onClick={() => loadAgent(agent.id)}
                    className={cn(
                      'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                      selectedAgent?.id === agent.id
                        ? 'bg-white/[0.08] text-white'
                        : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                    )}
                  >
                    <p className="text-sm font-medium truncate">{agent.name}</p>
                    <p className="text-xs text-white/40 truncate">{agent.model_id}</p>
                  </button>
                ))
              )}
            </div>
          </div>

          {/* Agent Editor */}
          <div className="flex-1 min-h-0 flex flex-col overflow-hidden">
            {editedAgent && selectedAgent ? (
              <>
                <div className="p-4 border-b border-white/[0.06] flex items-center justify-between">
                  <div className="min-w-0 flex-1">
                    <input
                      type="text"
                      value={editedAgent.name}
                      onChange={(e) => {
                        setEditedAgent({ ...editedAgent, name: e.target.value });
                        setDirty(true);
                      }}
                      className="text-lg font-medium text-white bg-transparent border-none outline-none w-full"
                    />
                    <p className="text-xs text-white/40">Agent Configuration</p>
                  </div>
                  <div className="flex items-center gap-2">
                    {dirty && <span className="text-xs text-amber-400">Unsaved</span>}
                    <button
                      onClick={handleDeleteAgent}
                      className="p-2 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                    <button
                      onClick={handleSaveAgent}
                      disabled={saving || !dirty}
                      className={cn(
                        'flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg transition-colors',
                        dirty
                          ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                          : 'text-white/40 bg-white/[0.04]'
                      )}
                    >
                      <Save className={cn('h-4 w-4', saving && 'animate-pulse')} />
                      Save
                    </button>
                  </div>
                </div>

                <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-6">
                  {/* Model Selection */}
                  <div>
                    <div className="flex items-center gap-2 mb-3">
                      <Cpu className="h-4 w-4 text-white/60" />
                      <h3 className="text-sm font-medium text-white">Model</h3>
                    </div>
                    <select
                      value={editedAgent.model_id}
                      onChange={(e) => {
                        setEditedAgent({ ...editedAgent, model_id: e.target.value });
                        setDirty(true);
                      }}
                      className="w-full px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white text-sm focus:outline-none focus:border-indigo-500/50"
                    >
                      {providers.map((provider) => (
                        <optgroup key={provider.id} label={provider.name}>
                          {provider.models.map((model) => (
                            <option key={model.id} value={model.id}>
                              {model.name}
                              {model.description && ` — ${model.description}`}
                            </option>
                          ))}
                        </optgroup>
                      ))}
                    </select>
                  </div>

                  {/* MCP Servers */}
                  <div>
                    <div className="flex items-center gap-2 mb-3">
                      <Settings className="h-4 w-4 text-white/60" />
                      <h3 className="text-sm font-medium text-white">MCP Servers</h3>
                    </div>
                    <div className="space-y-1">
                      {mcpServers.length === 0 ? (
                        <p className="text-xs text-white/40 py-2">No MCP servers configured</p>
                      ) : (
                        mcpServers.map((server) => (
                          <label
                            key={server.name}
                            className="flex items-center gap-2 p-2 rounded-lg hover:bg-white/[0.04] cursor-pointer"
                          >
                            <input
                              type="checkbox"
                              checked={editedAgent.mcp_servers.includes(server.name)}
                              onChange={() => toggleMcpServer(server.name)}
                              className="rounded border-white/20 bg-white/5 text-indigo-500 focus:ring-indigo-500/50"
                            />
                            <span className="text-sm text-white/80">{server.name}</span>
                          </label>
                        ))
                      )}
                    </div>
                  </div>

                  {/* Skills */}
                  <div>
                    <div className="flex items-center gap-2 mb-3">
                      <FileText className="h-4 w-4 text-white/60" />
                      <h3 className="text-sm font-medium text-white">Skills</h3>
                    </div>
                    <div className="space-y-1">
                      {skills.length === 0 ? (
                        <p className="text-xs text-white/40 py-2">No skills in library</p>
                      ) : (
                        skills.map((skill) => (
                          <label
                            key={skill.name}
                            className="flex items-center gap-2 p-2 rounded-lg hover:bg-white/[0.04] cursor-pointer"
                          >
                            <input
                              type="checkbox"
                              checked={editedAgent.skills.includes(skill.name)}
                              onChange={() => toggleSkill(skill.name)}
                              className="rounded border-white/20 bg-white/5 text-indigo-500 focus:ring-indigo-500/50"
                            />
                            <div className="flex-1 min-w-0">
                              <p className="text-sm text-white/80 truncate">{skill.name}</p>
                              {skill.description && (
                                <p className="text-xs text-white/40 truncate">{skill.description}</p>
                              )}
                            </div>
                          </label>
                        ))
                      )}
                    </div>
                  </div>

                  {/* Commands */}
                  <div>
                    <div className="flex items-center gap-2 mb-3">
                      <Terminal className="h-4 w-4 text-white/60" />
                      <h3 className="text-sm font-medium text-white">Commands</h3>
                    </div>
                    <div className="space-y-1">
                      {commands.length === 0 ? (
                        <p className="text-xs text-white/40 py-2">No commands in library</p>
                      ) : (
                        commands.map((command) => (
                          <label
                            key={command.name}
                            className="flex items-center gap-2 p-2 rounded-lg hover:bg-white/[0.04] cursor-pointer"
                          >
                            <input
                              type="checkbox"
                              checked={editedAgent.commands.includes(command.name)}
                              onChange={() => toggleCommand(command.name)}
                              className="rounded border-white/20 bg-white/5 text-indigo-500 focus:ring-indigo-500/50"
                            />
                            <div className="flex-1 min-w-0">
                              <p className="text-sm text-white/80 truncate">/{command.name}</p>
                              {command.description && (
                                <p className="text-xs text-white/40 truncate">{command.description}</p>
                              )}
                            </div>
                          </label>
                        ))
                      )}
                    </div>
                  </div>
                </div>
              </>
            ) : (
              <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
                Select an agent to configure
              </div>
            )}
          </div>
        </div>
      </div>

      {/* New Agent Dialog */}
      {showNewAgentDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Agent</h3>
            <div className="space-y-4">
              <div>
                <label className="text-xs text-white/60 mb-1 block">Name</label>
                <input
                  type="text"
                  placeholder="My Agent"
                  value={newAgentName}
                  onChange={(e) => setNewAgentName(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="text-xs text-white/60 mb-1 block">Model</label>
                <select
                  value={newAgentModel}
                  onChange={(e) => setNewAgentModel(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  {providers.map((provider) => (
                    <optgroup key={provider.id} label={provider.name}>
                      {provider.models.map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.name}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                </select>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => setShowNewAgentDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateAgent}
                disabled={!newAgentName.trim() || !newAgentModel.trim() || saving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {saving ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
