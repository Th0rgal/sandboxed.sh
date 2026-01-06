'use client';

import { useState, useRef, useEffect } from 'react';
import {
  getLibrarySkill,
  getLibraryCommand,
  type Skill,
  type Command,
} from '@/lib/api';
import { LibraryUnavailable } from '@/components/library-unavailable';
import {
  GitBranch,
  RefreshCw,
  Upload,
  Check,
  AlertCircle,
  FileCode,
  Terminal,
  Plug,
  ChevronDown,
  Loader,
  Plus,
  Save,
  Trash2,
  X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useLibrary } from '@/contexts/library-context';

type Section = 'mcps' | 'skills' | 'commands';

export default function LibraryPage() {
  const {
    status,
    mcps,
    skills,
    commands,
    loading,
    error,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
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
  } = useLibrary();

  // Section expansion state
  const [expandedSection, setExpandedSection] = useState<Section | null>(null);

  // MCPs state (local editing)
  const [mcpJsonContent, setMcpJsonContent] = useState(() => JSON.stringify(mcps, null, 2));
  const [mcpParseError, setMcpParseError] = useState<string | null>(null);
  const [mcpDirty, setMcpDirty] = useState(false);
  const [mcpSaving, setMcpSaving] = useState(false);

  // Skills state
  const [selectedSkill, setSelectedSkill] = useState<Skill | null>(null);
  const [skillContent, setSkillContent] = useState('');
  const [skillDirty, setSkillDirty] = useState(false);
  const [skillSaving, setSkillSaving] = useState(false);
  const [loadingSkill, setLoadingSkill] = useState(false);
  const [showNewSkillDialog, setShowNewSkillDialog] = useState(false);
  const [newSkillName, setNewSkillName] = useState('');

  // Commands state
  const [selectedCommand, setSelectedCommand] = useState<Command | null>(null);
  const [commandContent, setCommandContent] = useState('');
  const [commandDirty, setCommandDirty] = useState(false);
  const [commandSaving, setCommandSaving] = useState(false);
  const [loadingCommand, setLoadingCommand] = useState(false);
  const [showNewCommandDialog, setShowNewCommandDialog] = useState(false);
  const [newCommandName, setNewCommandName] = useState('');

  // Git dialog
  const [commitMessage, setCommitMessage] = useState('');
  const [showCommitDialog, setShowCommitDialog] = useState(false);

  // Refs for dirty flag comparison
  const mcpJsonContentRef = useRef(mcpJsonContent);
  mcpJsonContentRef.current = mcpJsonContent;
  const skillContentRef = useRef(skillContent);
  skillContentRef.current = skillContent;
  const commandContentRef = useRef(commandContent);
  commandContentRef.current = commandContent;

  // Handle Escape key for dialogs
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showCommitDialog) setShowCommitDialog(false);
        if (showNewSkillDialog) setShowNewSkillDialog(false);
        if (showNewCommandDialog) setShowNewCommandDialog(false);
      }
    };
    if (showCommitDialog || showNewSkillDialog || showNewCommandDialog) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [showCommitDialog, showNewSkillDialog, showNewCommandDialog]);

  // Update MCP content when mcps from context changes
  // This ensures the textarea reflects server state after saves
  const prevMcpsRef = useRef(mcps);
  if (prevMcpsRef.current !== mcps && !mcpDirty) {
    setMcpJsonContent(JSON.stringify(mcps, null, 2));
    prevMcpsRef.current = mcps;
  }

  // Git operations
  const handleSync = async () => {
    try {
      await sync();
      setMcpJsonContent(JSON.stringify(mcps, null, 2));
      setMcpDirty(false);
    } catch {
      // Error handled by context
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } catch {
      // Error handled by context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error handled by context
    }
  };

  // MCPs operations
  const handleMcpContentChange = (value: string) => {
    setMcpJsonContent(value);
    setMcpDirty(true);
    setMcpParseError(null);
    try {
      JSON.parse(value);
    } catch (err) {
      setMcpParseError(err instanceof Error ? err.message : 'Invalid JSON');
    }
  };

  const handleMcpSave = async () => {
    if (mcpParseError) return;
    const contentBeingSaved = mcpJsonContent;
    try {
      setMcpSaving(true);
      const parsed = JSON.parse(contentBeingSaved);
      await saveMcps(parsed);
      if (mcpJsonContentRef.current === contentBeingSaved) {
        setMcpDirty(false);
      }
    } catch (err) {
      console.error('Failed to save MCPs:', err);
    } finally {
      setMcpSaving(false);
    }
  };

  // Skills operations
  const loadSkill = async (name: string) => {
    try {
      setLoadingSkill(true);
      const skill = await getLibrarySkill(name);
      setSelectedSkill(skill);
      setSkillContent(skill.content);
      setSkillDirty(false);
    } catch (err) {
      console.error('Failed to load skill:', err);
    } finally {
      setLoadingSkill(false);
    }
  };

  const handleSkillSave = async () => {
    if (!selectedSkill) return;
    const contentBeingSaved = skillContent;
    try {
      setSkillSaving(true);
      await saveSkill(selectedSkill.name, contentBeingSaved);
      if (skillContentRef.current === contentBeingSaved) {
        setSkillDirty(false);
      }
    } catch (err) {
      console.error('Failed to save skill:', err);
    } finally {
      setSkillSaving(false);
    }
  };

  const handleSkillCreate = async () => {
    if (!newSkillName.trim()) return;
    const template = `---
description: A new skill
---

# ${newSkillName}

Describe what this skill does.
`;
    try {
      setSkillSaving(true);
      await saveSkill(newSkillName, template);
      setShowNewSkillDialog(false);
      setNewSkillName('');
      await loadSkill(newSkillName);
    } catch (err) {
      console.error('Failed to create skill:', err);
    } finally {
      setSkillSaving(false);
    }
  };

  const handleSkillDelete = async () => {
    if (!selectedSkill) return;
    if (!confirm(`Delete skill "${selectedSkill.name}"?`)) return;
    try {
      await removeSkill(selectedSkill.name);
      setSelectedSkill(null);
      setSkillContent('');
    } catch (err) {
      console.error('Failed to delete skill:', err);
    }
  };

  // Commands operations
  const loadCommand = async (name: string) => {
    try {
      setLoadingCommand(true);
      const command = await getLibraryCommand(name);
      setSelectedCommand(command);
      setCommandContent(command.content);
      setCommandDirty(false);
    } catch (err) {
      console.error('Failed to load command:', err);
    } finally {
      setLoadingCommand(false);
    }
  };

  const handleCommandSave = async () => {
    if (!selectedCommand) return;
    const contentBeingSaved = commandContent;
    try {
      setCommandSaving(true);
      await saveCommand(selectedCommand.name, contentBeingSaved);
      if (commandContentRef.current === contentBeingSaved) {
        setCommandDirty(false);
      }
    } catch (err) {
      console.error('Failed to save command:', err);
    } finally {
      setCommandSaving(false);
    }
  };

  const handleCommandCreate = async () => {
    if (!newCommandName.trim()) return;
    const template = `---
description: A new command
---

Describe what this command does.
`;
    try {
      setCommandSaving(true);
      await saveCommand(newCommandName, template);
      setShowNewCommandDialog(false);
      setNewCommandName('');
      await loadCommand(newCommandName);
    } catch (err) {
      console.error('Failed to create command:', err);
    } finally {
      setCommandSaving(false);
    }
  };

  const handleCommandDelete = async () => {
    if (!selectedCommand) return;
    if (!confirm(`Delete command "${selectedCommand.name}"?`)) return;
    try {
      await removeCommand(selectedCommand.name);
      setSelectedCommand(null);
      setCommandContent('');
    } catch (err) {
      console.error('Failed to delete command:', err);
    }
  };

  const toggleSection = (section: Section) => {
    setExpandedSection(expandedSection === section ? null : section);
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-4xl mx-auto">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold text-white mb-2">Library</h1>
        <p className="text-white/50">
          Manage your configuration library: MCP servers, skills, and commands.
        </p>
      </div>

      {libraryUnavailable ? (
        <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />
      ) : (
        <>
          {error && (
            <div className="mb-6 p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
              <AlertCircle className="h-4 w-4 flex-shrink-0" />
              {error}
              <button onClick={clearError} className="ml-auto">
                <X className="h-4 w-4" />
              </button>
            </div>
          )}

          {/* Git Status Bar */}
          {status && (
            <div className="mb-6 p-4 rounded-xl bg-white/[0.02] border border-white/[0.06]">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-4">
                  <div className="flex items-center gap-2">
                    <GitBranch className="h-4 w-4 text-white/40" />
                    <span className="text-sm font-medium text-white">{status.branch}</span>
                  </div>
                  <div className="flex items-center gap-2">
                    {status.clean ? (
                      <span className="flex items-center gap-1 text-xs text-emerald-400">
                        <Check className="h-3 w-3" />
                        Clean
                      </span>
                    ) : (
                      <span className="flex items-center gap-1 text-xs text-amber-400">
                        <AlertCircle className="h-3 w-3" />
                        {status.modified_files.length} modified
                      </span>
                    )}
                  </div>
                  {(status.ahead > 0 || status.behind > 0) && (
                    <div className="text-xs text-white/40">
                      {status.ahead > 0 && <span className="text-emerald-400">+{status.ahead}</span>}
                      {status.ahead > 0 && status.behind > 0 && ' / '}
                      {status.behind > 0 && <span className="text-amber-400">-{status.behind}</span>}
                    </div>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleSync}
                    disabled={syncing}
                    className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                  >
                    <RefreshCw className={cn('h-3 w-3', syncing && 'animate-spin')} />
                    Sync
                  </button>
                  {!status.clean && (
                    <button
                      onClick={() => setShowCommitDialog(true)}
                      disabled={committing}
                      className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                    >
                      <Check className="h-3 w-3" />
                      Commit
                    </button>
                  )}
                  {status.ahead > 0 && (
                    <button
                      onClick={handlePush}
                      disabled={pushing}
                      className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-emerald-400 hover:text-emerald-300 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors disabled:opacity-50"
                    >
                      <Upload className={cn('h-3 w-3', pushing && 'animate-pulse')} />
                      Push
                    </button>
                  )}
                </div>
              </div>
            </div>
          )}

          {/* Accordion Sections */}
          <div className="space-y-2">
            {/* MCP Servers Section */}
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
              <button
                onClick={() => toggleSection('mcps')}
                className="w-full p-4 flex items-center justify-between hover:bg-white/[0.02] transition-colors"
              >
                <div className="flex items-center gap-4">
                  <div className="p-2.5 rounded-lg bg-indigo-500/10">
                    <Plug className="h-5 w-5 text-indigo-400" />
                  </div>
                  <div className="text-left">
                    <h3 className="text-base font-medium text-white">MCP Servers</h3>
                    <p className="text-sm text-white/40">Configure Model Context Protocol servers</p>
                  </div>
                </div>
                <ChevronDown
                  className={cn(
                    'h-5 w-5 text-white/40 transition-transform duration-200',
                    expandedSection === 'mcps' && 'rotate-180'
                  )}
                />
              </button>
              {expandedSection === 'mcps' && (
                <div className="border-t border-white/[0.06]">
                  <div className="p-4 border-b border-white/[0.06] flex items-center justify-between">
                    <span className="text-xs text-white/40">mcp/servers.json</span>
                    <div className="flex items-center gap-2">
                      {mcpDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                      <button
                        onClick={handleMcpSave}
                        disabled={mcpSaving || !!mcpParseError || !mcpDirty}
                        className={cn(
                          'flex items-center gap-2 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                          mcpDirty && !mcpParseError
                            ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                            : 'text-white/40 bg-white/[0.04]'
                        )}
                      >
                        <Save className={cn('h-3 w-3', mcpSaving && 'animate-pulse')} />
                        Save
                      </button>
                    </div>
                  </div>
                  {mcpParseError && (
                    <div className="mx-4 mt-4 p-2 rounded-lg bg-amber-500/10 text-amber-400 text-xs flex items-center gap-2">
                      <AlertCircle className="h-3 w-3" />
                      {mcpParseError}
                    </div>
                  )}
                  <div className="p-4">
                    <textarea
                      value={mcpJsonContent}
                      onChange={(e) => handleMcpContentChange(e.target.value)}
                      className="w-full h-64 font-mono text-sm bg-[#0d0d0e] border border-white/[0.06] rounded-lg p-4 text-white/90 resize-none focus:outline-none focus:border-indigo-500/50"
                      spellCheck={false}
                    />
                  </div>
                </div>
              )}
            </div>

            {/* Skills Section */}
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
              <button
                onClick={() => toggleSection('skills')}
                className="w-full p-4 flex items-center justify-between hover:bg-white/[0.02] transition-colors"
              >
                <div className="flex items-center gap-4">
                  <div className="p-2.5 rounded-lg bg-emerald-500/10">
                    <FileCode className="h-5 w-5 text-emerald-400" />
                  </div>
                  <div className="text-left">
                    <h3 className="text-base font-medium text-white">Skills</h3>
                    <p className="text-sm text-white/40">
                      {skills.length} skill{skills.length !== 1 ? 's' : ''} available
                    </p>
                  </div>
                </div>
                <ChevronDown
                  className={cn(
                    'h-5 w-5 text-white/40 transition-transform duration-200',
                    expandedSection === 'skills' && 'rotate-180'
                  )}
                />
              </button>
              {expandedSection === 'skills' && (
                <div className="border-t border-white/[0.06]">
                  <div className="flex h-80">
                    {/* Skills List */}
                    <div className="w-56 border-r border-white/[0.06] flex flex-col">
                      <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                        <span className="text-xs font-medium text-white/60">Skills</span>
                        <button
                          onClick={() => setShowNewSkillDialog(true)}
                          className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                        >
                          <Plus className="h-3.5 w-3.5 text-white/60" />
                        </button>
                      </div>
                      <div className="flex-1 overflow-y-auto p-2">
                        {skills.length === 0 ? (
                          <p className="text-xs text-white/40 text-center py-4">No skills yet</p>
                        ) : (
                          skills.map((skill) => (
                            <button
                              key={skill.name}
                              onClick={() => loadSkill(skill.name)}
                              className={cn(
                                'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                                selectedSkill?.name === skill.name
                                  ? 'bg-white/[0.08] text-white'
                                  : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                              )}
                            >
                              <p className="text-sm font-medium truncate">{skill.name}</p>
                              {skill.description && (
                                <p className="text-xs text-white/40 truncate">{skill.description}</p>
                              )}
                            </button>
                          ))
                        )}
                      </div>
                    </div>

                    {/* Skills Editor */}
                    <div className="flex-1 flex flex-col">
                      {selectedSkill ? (
                        <>
                          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                            <div className="min-w-0">
                              <p className="text-sm font-medium text-white truncate">{selectedSkill.name}</p>
                              <p className="text-xs text-white/40">{selectedSkill.path}/SKILL.md</p>
                            </div>
                            <div className="flex items-center gap-2">
                              {skillDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                              <button
                                onClick={handleSkillDelete}
                                className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                              </button>
                              <button
                                onClick={handleSkillSave}
                                disabled={skillSaving || !skillDirty}
                                className={cn(
                                  'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                                  skillDirty
                                    ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                                    : 'text-white/40 bg-white/[0.04]'
                                )}
                              >
                                <Save className={cn('h-3 w-3', skillSaving && 'animate-pulse')} />
                                Save
                              </button>
                            </div>
                          </div>
                          <div className="flex-1 p-3 overflow-hidden">
                            {loadingSkill ? (
                              <div className="flex items-center justify-center h-full">
                                <Loader className="h-5 w-5 animate-spin text-white/40" />
                              </div>
                            ) : (
                              <textarea
                                value={skillContent}
                                onChange={(e) => {
                                  setSkillContent(e.target.value);
                                  setSkillDirty(true);
                                }}
                                className="w-full h-full font-mono text-xs bg-[#0d0d0e] border border-white/[0.06] rounded-lg p-3 text-white/90 resize-none focus:outline-none focus:border-indigo-500/50"
                                spellCheck={false}
                              />
                            )}
                          </div>
                        </>
                      ) : (
                        <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
                          Select a skill to edit
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              )}
            </div>

            {/* Commands Section */}
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
              <button
                onClick={() => toggleSection('commands')}
                className="w-full p-4 flex items-center justify-between hover:bg-white/[0.02] transition-colors"
              >
                <div className="flex items-center gap-4">
                  <div className="p-2.5 rounded-lg bg-amber-500/10">
                    <Terminal className="h-5 w-5 text-amber-400" />
                  </div>
                  <div className="text-left">
                    <h3 className="text-base font-medium text-white">Commands</h3>
                    <p className="text-sm text-white/40">
                      {commands.length} command{commands.length !== 1 ? 's' : ''} available
                    </p>
                  </div>
                </div>
                <ChevronDown
                  className={cn(
                    'h-5 w-5 text-white/40 transition-transform duration-200',
                    expandedSection === 'commands' && 'rotate-180'
                  )}
                />
              </button>
              {expandedSection === 'commands' && (
                <div className="border-t border-white/[0.06]">
                  <div className="flex h-80">
                    {/* Commands List */}
                    <div className="w-56 border-r border-white/[0.06] flex flex-col">
                      <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                        <span className="text-xs font-medium text-white/60">Commands</span>
                        <button
                          onClick={() => setShowNewCommandDialog(true)}
                          className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                        >
                          <Plus className="h-3.5 w-3.5 text-white/60" />
                        </button>
                      </div>
                      <div className="flex-1 overflow-y-auto p-2">
                        {commands.length === 0 ? (
                          <p className="text-xs text-white/40 text-center py-4">No commands yet</p>
                        ) : (
                          commands.map((command) => (
                            <button
                              key={command.name}
                              onClick={() => loadCommand(command.name)}
                              className={cn(
                                'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                                selectedCommand?.name === command.name
                                  ? 'bg-white/[0.08] text-white'
                                  : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                              )}
                            >
                              <p className="text-sm font-medium truncate">/{command.name}</p>
                              {command.description && (
                                <p className="text-xs text-white/40 truncate">{command.description}</p>
                              )}
                            </button>
                          ))
                        )}
                      </div>
                    </div>

                    {/* Commands Editor */}
                    <div className="flex-1 flex flex-col">
                      {selectedCommand ? (
                        <>
                          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                            <div className="min-w-0">
                              <p className="text-sm font-medium text-white truncate">/{selectedCommand.name}</p>
                              <p className="text-xs text-white/40">{selectedCommand.path}</p>
                            </div>
                            <div className="flex items-center gap-2">
                              {commandDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                              <button
                                onClick={handleCommandDelete}
                                className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                              </button>
                              <button
                                onClick={handleCommandSave}
                                disabled={commandSaving || !commandDirty}
                                className={cn(
                                  'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                                  commandDirty
                                    ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                                    : 'text-white/40 bg-white/[0.04]'
                                )}
                              >
                                <Save className={cn('h-3 w-3', commandSaving && 'animate-pulse')} />
                                Save
                              </button>
                            </div>
                          </div>
                          <div className="flex-1 p-3 overflow-hidden">
                            {loadingCommand ? (
                              <div className="flex items-center justify-center h-full">
                                <Loader className="h-5 w-5 animate-spin text-white/40" />
                              </div>
                            ) : (
                              <textarea
                                value={commandContent}
                                onChange={(e) => {
                                  setCommandContent(e.target.value);
                                  setCommandDirty(true);
                                }}
                                className="w-full h-full font-mono text-xs bg-[#0d0d0e] border border-white/[0.06] rounded-lg p-3 text-white/90 resize-none focus:outline-none focus:border-indigo-500/50"
                                spellCheck={false}
                              />
                            )}
                          </div>
                        </>
                      ) : (
                        <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
                          Select a command to edit
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              )}
            </div>
          </div>
        </>
      )}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Commit Changes</h3>
            <input
              type="text"
              placeholder="Commit message..."
              value={commitMessage}
              onChange={(e) => setCommitMessage(e.target.value)}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowCommitDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCommit}
                disabled={!commitMessage.trim() || committing}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {committing ? 'Committing...' : 'Commit'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New Skill Dialog */}
      {showNewSkillDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Skill</h3>
            <input
              type="text"
              placeholder="Skill name (e.g., my-skill)"
              value={newSkillName}
              onChange={(e) => setNewSkillName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowNewSkillDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleSkillCreate}
                disabled={!newSkillName.trim() || skillSaving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {skillSaving ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New Command Dialog */}
      {showNewCommandDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Command</h3>
            <input
              type="text"
              placeholder="Command name (e.g., my-command)"
              value={newCommandName}
              onChange={(e) => setNewCommandName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowNewCommandDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCommandCreate}
                disabled={!newCommandName.trim() || commandSaving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {commandSaving ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
