'use client';

import { useState, useRef, useEffect } from 'react';
import { getLibraryRule, type Rule } from '@/lib/api';
import {
  GitBranch,
  RefreshCw,
  Upload,
  Check,
  AlertCircle,
  Loader,
  Plus,
  Save,
  Trash2,
  X,
  FileText,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';
import { ConfigCodeEditor } from '@/components/config-code-editor';

export default function RulesPage() {
  const {
    status,
    rules,
    loading,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    sync,
    commit,
    push,
    saveRule,
    removeRule,
    syncing,
    committing,
    pushing,
  } = useLibrary();

  const [selectedRule, setSelectedRule] = useState<Rule | null>(null);
  const [ruleContent, setRuleContent] = useState('');
  const [ruleDirty, setRuleDirty] = useState(false);
  const [ruleSaving, setRuleSaving] = useState(false);
  const [loadingRule, setLoadingRule] = useState(false);
  const [showNewRuleDialog, setShowNewRuleDialog] = useState(false);
  const [newRuleName, setNewRuleName] = useState('');
  const [commitMessage, setCommitMessage] = useState('');
  const [showCommitDialog, setShowCommitDialog] = useState(false);

  // Ref to track content for dirty flag comparison
  const ruleContentRef = useRef(ruleContent);
  ruleContentRef.current = ruleContent;

  // Handle Escape key for dialogs
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showCommitDialog) setShowCommitDialog(false);
        if (showNewRuleDialog) setShowNewRuleDialog(false);
      }
    };
    if (showCommitDialog || showNewRuleDialog) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [showCommitDialog, showNewRuleDialog]);

  // Handle keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        if (ruleDirty && selectedRule) {
          handleRuleSave();
        }
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [ruleDirty, selectedRule]);

  const handleSync = async () => {
    try {
      await sync();
    } catch {
      // Error is handled by context
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } catch {
      // Error is handled by context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error is handled by context
    }
  };

  const loadRule = async (name: string) => {
    try {
      setLoadingRule(true);
      const rule = await getLibraryRule(name);
      setSelectedRule(rule);
      setRuleContent(rule.content);
      setRuleDirty(false);
    } catch (err) {
      console.error('Failed to load rule:', err);
    } finally {
      setLoadingRule(false);
    }
  };

  const handleRuleSave = async () => {
    if (!selectedRule) return;
    const contentBeingSaved = ruleContent;
    try {
      setRuleSaving(true);
      await saveRule(selectedRule.name, contentBeingSaved);
      // Only clear dirty if content hasn't changed during save
      if (ruleContentRef.current === contentBeingSaved) {
        setRuleDirty(false);
      }
    } catch (err) {
      console.error('Failed to save rule:', err);
    } finally {
      setRuleSaving(false);
    }
  };

  const handleRuleCreate = async () => {
    if (!newRuleName.trim()) return;
    const template = `---
description: A new rule
---

# ${newRuleName.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}

Describe what this rule does.

## Guidelines

- Guideline 1
- Guideline 2
`;
    try {
      setRuleSaving(true);
      await saveRule(newRuleName, template);
      setShowNewRuleDialog(false);
      setNewRuleName('');
      await loadRule(newRuleName);
    } catch (err) {
      console.error('Failed to create rule:', err);
    } finally {
      setRuleSaving(false);
    }
  };

  const handleRuleDelete = async () => {
    if (!selectedRule) return;
    if (!confirm(`Delete rule "${selectedRule.name}"?`)) return;
    try {
      await removeRule(selectedRule.name);
      setSelectedRule(null);
      setRuleContent('');
    } catch (err) {
      console.error('Failed to delete rule:', err);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-7xl mx-auto space-y-4">
      {libraryUnavailable ? (
        <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />
      ) : (
        <>
          {/* Git Status Bar */}
          {status && (
            <div className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.06]">
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
                      {status.ahead > 0 && (
                        <span className="text-emerald-400">+{status.ahead}</span>
                      )}
                      {status.ahead > 0 && status.behind > 0 && ' / '}
                      {status.behind > 0 && (
                        <span className="text-amber-400">-{status.behind}</span>
                      )}
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

          {/* Rules Editor */}
          <div className="flex-1 min-h-0 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden flex flex-col">
            <div className="flex flex-1 min-h-0">
              {/* Rules List */}
              <div className="w-64 border-r border-white/[0.06] flex flex-col min-h-0">
                <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                  <span className="text-xs font-medium text-white/60">
                    Rules{rules.length ? ` (${rules.length})` : ''}
                  </span>
                  <button
                    onClick={() => setShowNewRuleDialog(true)}
                    className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                    title="New Rule"
                  >
                    <Plus className="h-3.5 w-3.5 text-white/60" />
                  </button>
                </div>
                <div className="flex-1 min-h-0 overflow-y-auto p-2">
                  {rules.length === 0 ? (
                    <div className="text-center py-8">
                      <FileText className="h-8 w-8 text-white/20 mx-auto mb-2" />
                      <p className="text-xs text-white/40 mb-3">No rules yet</p>
                      <button
                        onClick={() => setShowNewRuleDialog(true)}
                        className="text-xs text-indigo-400 hover:text-indigo-300"
                      >
                        Create your first rule
                      </button>
                    </div>
                  ) : (
                    rules.map((rule) => (
                      <button
                        key={rule.name}
                        onClick={() => loadRule(rule.name)}
                        className={cn(
                          'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                          selectedRule?.name === rule.name
                            ? 'bg-white/[0.08] text-white'
                            : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                        )}
                      >
                        <p className="text-sm font-medium truncate">{rule.name}</p>
                        {rule.description && (
                          <p className="text-xs text-white/40 truncate">{rule.description}</p>
                        )}
                      </button>
                    ))
                  )}
                </div>
              </div>

              {/* Rules Editor */}
              <div className="flex-1 min-h-0 flex flex-col">
                {selectedRule ? (
                  <>
                    <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                      <div className="min-w-0">
                        <p className="text-sm font-medium text-white truncate">
                          {selectedRule.name}
                        </p>
                        <p className="text-xs text-white/40">{selectedRule.path}</p>
                      </div>
                      <div className="flex items-center gap-2">
                        {ruleDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                        <button
                          onClick={handleRuleDelete}
                          className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                          title="Delete Rule"
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </button>
                        <button
                          onClick={handleRuleSave}
                          disabled={ruleSaving || !ruleDirty}
                          className={cn(
                            'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                            ruleDirty
                              ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                              : 'text-white/40 bg-white/[0.04]'
                          )}
                        >
                          <Save className={cn('h-3 w-3', ruleSaving && 'animate-pulse')} />
                          Save
                        </button>
                      </div>
                    </div>
                    <div className="flex-1 min-h-0 p-3 overflow-hidden">
                      {loadingRule ? (
                        <div className="flex items-center justify-center h-full">
                          <Loader className="h-5 w-5 animate-spin text-white/40" />
                        </div>
                      ) : (
                        <ConfigCodeEditor
                          value={ruleContent}
                          onChange={(value) => {
                            setRuleContent(value);
                            setRuleDirty(true);
                          }}
                          language="markdown"
                          className="h-full"
                          disabled={ruleSaving}
                          placeholder="---
description: Rule description
---

# Rule Title

Your rule content here..."
                        />
                      )}
                    </div>
                  </>
                ) : (
                  <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
                    Select a rule to edit or create a new one
                  </div>
                )}
              </div>
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

      {/* New Rule Dialog */}
      {showNewRuleDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Rule</h3>
            <input
              type="text"
              placeholder="Rule name (e.g., code-style)"
              value={newRuleName}
              onChange={(e) => setNewRuleName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-2"
            />
            <p className="text-xs text-white/40 mb-4">
              Rules are AGENTS.md-style markdown files that define coding standards, best practices, and instructions for agents.
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => {
                  setShowNewRuleDialog(false);
                  setNewRuleName('');
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleRuleCreate}
                disabled={!newRuleName.trim() || ruleSaving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {ruleSaving ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
