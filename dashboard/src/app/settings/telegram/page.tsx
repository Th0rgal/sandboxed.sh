'use client';

import { useState, useEffect, useMemo } from 'react';
import useSWR from 'swr';
import {
  listTelegramBots,
  createTelegramBot,
  updateTelegramChannel,
  deleteTelegramChannel,
  listBotChats,
  listBotActionExecutions,
  listBotScheduledMessages,
  listBotStructuredMemory,
  searchBotStructuredMemory,
  listMissions,
  type Mission,
  type TelegramActionExecution,
  type TelegramChannel,
  type TelegramChatMission,
  type TelegramScheduledMessage,
  type TelegramStructuredMemoryEntry,
  type TelegramStructuredMemorySearchHit,
  type TelegramTriggerMode,
  type CreateTelegramBotInput,
} from '@/lib/api';
import { listBackends, listWorkspaces, listBackendModelOptions, listProviders, listConfigProfiles, type Backend, type BackendModelOption, type Provider, type Workspace, type ConfigProfileSummary } from '@/lib/api';
import {
  MessageCircle,
  Plus,
  Trash2,
  Loader,
  Power,
  PowerOff,
  Bot,
  ChevronDown,
  ChevronUp,
  Settings,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast } from '@/components/toast';

const TRIGGER_MODE_LABELS: Record<TelegramTriggerMode, string> = {
  mention_or_dm: 'Mentions, replies & DMs',
  bot_mention: 'Bot @mentions only',
  reply: 'Replies to bot only',
  direct_message: 'Direct messages only',
  always: 'All messages (no filter)',
};

const BACKEND_LABELS: Record<string, string> = {
  claudecode: 'Claude Code',
  opencode: 'OpenCode',
  codex: 'Codex',
  gemini: 'Gemini',
  grok: 'Grok Build',
};

export default function TelegramSettingsPage() {
  const { data: bots = [], mutate: mutateBots } = useSWR('telegram-bots', listTelegramBots, {
    revalidateOnFocus: false,
  });
  const { data: backends = [] } = useSWR('backends', listBackends, {
    revalidateOnFocus: false,
  });
  const { data: workspaces = [] } = useSWR('workspaces', listWorkspaces, {
    revalidateOnFocus: false,
  });
  const { data: providersResponse } = useSWR(
    'model-providers',
    () => listProviders({ includeAll: true }),
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );
  const { data: backendModelOptions } = useSWR(
    'backend-model-options',
    () => listBackendModelOptions({ includeAll: true }),
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );
  const { data: missions = [] } = useSWR('missions', listMissions, {
    revalidateOnFocus: false,
  });
  const { data: configProfiles = [] } = useSWR('config-profiles', listConfigProfiles, {
    revalidateOnFocus: false,
  });

  // Chat mappings keyed by bot ID
  const [chatsByBot, setChatsByBot] = useState<Record<string, TelegramChatMission[]>>({});
  const [actionsByBot, setActionsByBot] = useState<Record<string, TelegramActionExecution[]>>({});
  const [scheduledByBot, setScheduledByBot] = useState<Record<string, TelegramScheduledMessage[]>>({});
  const [memoryByBot, setMemoryByBot] = useState<Record<string, TelegramStructuredMemoryEntry[]>>({});
  const [memorySearchByBot, setMemorySearchByBot] = useState<Record<string, TelegramStructuredMemorySearchHit[]>>({});
  const [memorySearchQueryByBot, setMemorySearchQueryByBot] = useState<Record<string, string>>({});
  const [expandedBots, setExpandedBots] = useState<Set<string>>(new Set());
  const [loadingChats, setLoadingChats] = useState<Set<string>>(new Set());
  const [loadingActions, setLoadingActions] = useState<Set<string>>(new Set());
  const [loadingScheduled, setLoadingScheduled] = useState<Set<string>>(new Set());
  const [loadingMemory, setLoadingMemory] = useState<Set<string>>(new Set());
  const [loadingMemorySearch, setLoadingMemorySearch] = useState<Set<string>>(new Set());

  // Create dialog
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [createBotToken, setCreateBotToken] = useState('');
  const [createBotUsername, setCreateBotUsername] = useState('');
  const [createTriggerMode, setCreateTriggerMode] = useState<TelegramTriggerMode>('mention_or_dm');
  const [createInstructions, setCreateInstructions] = useState('');
  const [createAllowedChatIds, setCreateAllowedChatIds] = useState('');
  const [createBackend, setCreateBackend] = useState('claudecode');
  const [createModelOverride, setCreateModelOverride] = useState('');
  const [createModelEffort, setCreateModelEffort] = useState('');
  const [createWorkspaceId, setCreateWorkspaceId] = useState('');
  const [createConfigProfile, setCreateConfigProfile] = useState('');
  const [creating, setCreating] = useState(false);

  // Model selector options helper
  const getModelOptionsForBackend = (backend: string) => {
    const allowlist =
      backend === 'claudecode' ? new Set(['anthropic']) :
      backend === 'codex' ? new Set(['openai']) :
      backend === 'gemini' ? new Set(['google']) :
      backend === 'grok' ? new Set(['xai']) : null;

    const backendOpts = backendModelOptions?.backends?.[backend];
    if (backendOpts && backendOpts.length > 0) {
      return backendOpts as BackendModelOption[];
    }
    const providers = (providersResponse?.providers || []) as Provider[];
    const options: Array<{ value: string; label: string; description?: string }> = [];
    for (const provider of providers) {
      if (allowlist && !allowlist.has(provider.id)) continue;
      for (const model of provider.models) {
        const value = backend === 'opencode' ? `${provider.id}/${model.id}` : model.id;
        options.push({ value, label: `${provider.name} — ${model.name}`, description: model.description });
      }
    }
    return options;
  };

  const modelOptions = useMemo(() => getModelOptionsForBackend(createBackend), [backendModelOptions, providersResponse, createBackend]);

  // Edit dialog
  const [editingBot, setEditingBot] = useState<TelegramChannel | null>(null);
  const [editInstructions, setEditInstructions] = useState('');
  const [editTriggerMode, setEditTriggerMode] = useState<TelegramTriggerMode>('mention_or_dm');
  const [editBackend, setEditBackend] = useState('');
  const [editModelOverride, setEditModelOverride] = useState('');
  const [editModelEffort, setEditModelEffort] = useState('');
  const [editWorkspaceId, setEditWorkspaceId] = useState('');
  const [editConfigProfile, setEditConfigProfile] = useState('');
  const [saving, setSaving] = useState(false);

  const editModelOptions = useMemo(() => getModelOptionsForBackend(editBackend || 'claudecode'), [backendModelOptions, providersResponse, editBackend]);

  const loadChats = async (botId: string) => {
    if (chatsByBot[botId]) return; // already loaded
    setLoadingChats((prev) => new Set(prev).add(botId));
    try {
      const chats = await listBotChats(botId);
      setChatsByBot((prev) => ({ ...prev, [botId]: chats }));
    } catch {
      // ignore
    } finally {
      setLoadingChats((prev) => {
        const next = new Set(prev);
        next.delete(botId);
        return next;
      });
    }
  };

  const loadScheduled = async (botId: string) => {
    if (scheduledByBot[botId]) return;
    setLoadingScheduled((prev) => new Set(prev).add(botId));
    try {
      const scheduled = await listBotScheduledMessages(botId, { limit: 8 });
      setScheduledByBot((prev) => ({ ...prev, [botId]: scheduled }));
    } catch {
      // ignore
    } finally {
      setLoadingScheduled((prev) => {
        const next = new Set(prev);
        next.delete(botId);
        return next;
      });
    }
  };

  const loadActions = async (botId: string) => {
    if (actionsByBot[botId]) return;
    setLoadingActions((prev) => new Set(prev).add(botId));
    try {
      const actions = await listBotActionExecutions(botId, { limit: 8 });
      setActionsByBot((prev) => ({ ...prev, [botId]: actions }));
    } catch {
      // ignore
    } finally {
      setLoadingActions((prev) => {
        const next = new Set(prev);
        next.delete(botId);
        return next;
      });
    }
  };

  const loadMemory = async (botId: string) => {
    if (memoryByBot[botId]) return;
    setLoadingMemory((prev) => new Set(prev).add(botId));
    try {
      const memory = await listBotStructuredMemory(botId, { limit: 8 });
      setMemoryByBot((prev) => ({ ...prev, [botId]: memory }));
    } catch {
      // ignore
    } finally {
      setLoadingMemory((prev) => {
        const next = new Set(prev);
        next.delete(botId);
        return next;
      });
    }
  };

  const loadMemorySearch = async (botId: string) => {
    const query = memorySearchQueryByBot[botId]?.trim();
    if (!query) {
      setMemorySearchByBot((prev) => ({ ...prev, [botId]: [] }));
      return;
    }
    setLoadingMemorySearch((prev) => new Set(prev).add(botId));
    try {
      const hits = await searchBotStructuredMemory(botId, { q: query, limit: 6 });
      setMemorySearchByBot((prev) => ({ ...prev, [botId]: hits }));
    } catch {
      // ignore
    } finally {
      setLoadingMemorySearch((prev) => {
        const next = new Set(prev);
        next.delete(botId);
        return next;
      });
    }
  };

  const toggleExpand = (botId: string) => {
    const wasExpanded = expandedBots.has(botId);
    setExpandedBots((prev) => {
      const next = new Set(prev);
      if (next.has(botId)) {
        next.delete(botId);
      } else {
        next.add(botId);
      }
      return next;
    });
    if (!wasExpanded) {
      void loadChats(botId);
      void loadActions(botId);
      void loadScheduled(botId);
      void loadMemory(botId);
    }
  };

  const handleCreate = async () => {
    if (!createBotToken.trim()) return;
    setCreating(true);
    try {
      const input: CreateTelegramBotInput = {
        bot_token: createBotToken.trim(),
      };
      if (createBotUsername.trim()) input.bot_username = createBotUsername.trim();
      if (createTriggerMode !== 'mention_or_dm') input.trigger_mode = createTriggerMode;
      if (createInstructions.trim()) input.instructions = createInstructions.trim();
      if (createAllowedChatIds.trim()) {
        input.allowed_chat_ids = createAllowedChatIds
          .split(',')
          .map((s) => parseInt(s.trim(), 10))
          .filter((n) => !isNaN(n));
      }
      if (createBackend) input.default_backend = createBackend;
      if (createModelOverride.trim()) input.default_model_override = createModelOverride.trim();
      if (createModelEffort) input.default_model_effort = createModelEffort;
      if (createWorkspaceId) input.default_workspace_id = createWorkspaceId;
      if (createConfigProfile.trim()) input.default_config_profile = createConfigProfile.trim();

      const bot = await createTelegramBot(input);
      await mutateBots();
      setShowCreateDialog(false);
      resetCreateForm();
      toast.success(`Bot @${bot.bot_username || 'bot'} created`);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to create bot');
    } finally {
      setCreating(false);
    }
  };

  const handleToggleActive = async (bot: TelegramChannel) => {
    try {
      await updateTelegramChannel(bot.id, { active: !bot.active });
      await mutateBots();
      toast.success(bot.active ? 'Bot deactivated' : 'Bot activated');
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to toggle bot');
    }
  };

  const handleDelete = async (bot: TelegramChannel) => {
    if (!confirm(`Delete bot @${bot.bot_username || bot.id.slice(0, 8)}?`)) return;
    try {
      await deleteTelegramChannel(bot.id);
      await mutateBots();
      toast.success('Bot deleted');
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to delete bot');
    }
  };

  const handleSaveEdit = async () => {
    if (!editingBot) return;
    setSaving(true);
    try {
      await updateTelegramChannel(editingBot.id, {
        instructions: editInstructions.trim() || '',
        trigger_mode: editTriggerMode,
        default_backend: editBackend || undefined,
        default_model_override: editModelOverride || undefined,
        default_model_effort: editModelEffort || undefined,
        default_workspace_id: editWorkspaceId || undefined,
        default_config_profile: editConfigProfile || undefined,
      });
      await mutateBots();
      setEditingBot(null);
      toast.success('Bot updated');
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to update bot');
    } finally {
      setSaving(false);
    }
  };

  const resetCreateForm = () => {
    setCreateBotToken('');
    setCreateBotUsername('');
    setCreateTriggerMode('mention_or_dm');
    setCreateInstructions('');
    setCreateAllowedChatIds('');
    setCreateBackend('claudecode');
    setCreateModelOverride('');
    setCreateModelEffort('');
    setCreateWorkspaceId('');
    setCreateConfigProfile('');
  };

  const getMissionTitle = (missionId: string) => {
    const m = missions.find((m: Mission) => m.id === missionId);
    return m?.title || missionId.slice(0, 8) + '...';
  };

  // ESC to close dialogs
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showCreateDialog) setShowCreateDialog(false);
        if (editingBot) setEditingBot(null);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showCreateDialog, editingBot]);

  return (
    <div className="flex-1 p-6 overflow-auto">
      <div className="max-w-4xl mx-auto space-y-8">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-semibold text-white mb-2">Telegram Bots</h1>
            <p className="text-white/50">
              Configure Telegram bots that auto-create missions for each conversation.
            </p>
          </div>
          <button
            onClick={() => setShowCreateDialog(true)}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
          >
            <Plus className="h-4 w-4" />
            Add Bot
          </button>
        </div>

        {/* Bot list */}
        {bots.length === 0 ? (
          <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-12 text-center">
            <MessageCircle className="h-12 w-12 text-white/20 mx-auto mb-4" />
            <h3 className="text-lg font-medium text-white mb-2">No Telegram bots</h3>
            <p className="text-sm text-white/50 mb-6 max-w-md mx-auto">
              Create a Telegram bot via @BotFather, then add it here. Each chat with the bot
              will automatically create a new mission.
            </p>
            <button
              onClick={() => setShowCreateDialog(true)}
              className="inline-flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
            >
              <Plus className="h-4 w-4" />
              Add Bot
            </button>
          </div>
        ) : (
          <div className="space-y-4">
            {bots.map((bot) => (
              <div
                key={bot.id}
                className="rounded-xl border border-white/[0.06] bg-white/[0.02] overflow-hidden"
              >
                <div className="p-4 flex items-center gap-4">
                  {/* Bot icon */}
                  <div
                    className={cn(
                      'flex h-10 w-10 items-center justify-center rounded-lg',
                      bot.active ? 'bg-emerald-500/10' : 'bg-white/[0.04]'
                    )}
                  >
                    <Bot
                      className={cn(
                        'h-5 w-5',
                        bot.active ? 'text-emerald-400' : 'text-white/40'
                      )}
                    />
                  </div>

                  {/* Info */}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium text-white">
                        @{bot.bot_username || 'unknown'}
                      </span>
                      <span
                        className={cn(
                          'inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium',
                          bot.active
                            ? 'bg-emerald-500/10 text-emerald-400'
                            : 'bg-white/[0.06] text-white/40'
                        )}
                      >
                        {bot.active ? 'Active' : 'Inactive'}
                      </span>
                      <span className="inline-flex items-center rounded bg-white/[0.06] px-1.5 py-0.5 text-[10px] text-white/40">
                        {TRIGGER_MODE_LABELS[bot.trigger_mode]}
                      </span>
                      {bot.auto_create_missions && (
                        <span className="inline-flex items-center rounded bg-indigo-500/10 border border-indigo-500/20 px-1.5 py-0.5 text-[10px] font-medium text-indigo-400">
                          Auto-create
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-3 mt-0.5">
                      <p className="text-xs text-white/40">
                        {BACKEND_LABELS[bot.default_backend || 'claudecode'] || bot.default_backend || 'Claude Code'}
                      </p>
                      {bot.default_model_override && (
                        <p className="text-xs text-white/30">{bot.default_model_override}</p>
                      )}
                      {chatsByBot[bot.id] && (
                        <p className="text-xs text-white/30">
                          {chatsByBot[bot.id].length} chat{chatsByBot[bot.id].length !== 1 ? 's' : ''}
                        </p>
                      )}
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="flex items-center gap-1">
                    <button
                      onClick={() => {
                        setEditingBot(bot);
                        setEditInstructions(bot.instructions || '');
                        setEditTriggerMode(bot.trigger_mode);
                        setEditBackend(bot.default_backend || 'claudecode');
                        setEditModelOverride(bot.default_model_override || '');
                        setEditModelEffort(bot.default_model_effort || '');
                        setEditWorkspaceId(bot.default_workspace_id || '');
                        setEditConfigProfile(bot.default_config_profile || '');
                      }}
                      className="p-2 rounded-lg text-white/40 hover:text-white hover:bg-white/[0.06] transition-colors"
                      title="Edit"
                    >
                      <Settings className="h-4 w-4" />
                    </button>
                    <button
                      onClick={() => handleToggleActive(bot)}
                      className={cn(
                        'p-2 rounded-lg transition-colors',
                        bot.active
                          ? 'text-emerald-400/60 hover:text-emerald-400 hover:bg-emerald-500/10'
                          : 'text-white/40 hover:text-white hover:bg-white/[0.06]'
                      )}
                      title={bot.active ? 'Deactivate' : 'Activate'}
                    >
                      {bot.active ? (
                        <Power className="h-4 w-4" />
                      ) : (
                        <PowerOff className="h-4 w-4" />
                      )}
                    </button>
                    <button
                      onClick={() => handleDelete(bot)}
                      className="p-2 rounded-lg text-red-400/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                      title="Delete"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  </div>
                </div>

                {/* Expandable details - show chats */}
                <button
                  onClick={() => toggleExpand(bot.id)}
                  className="w-full flex items-center justify-center gap-1 py-1.5 border-t border-white/[0.04] text-[10px] text-white/30 hover:text-white/50 hover:bg-white/[0.02] transition-colors"
                >
                  {expandedBots.has(bot.id) ? (
                    <>
                      <ChevronUp className="h-3 w-3" /> Less
                    </>
                  ) : (
                    <>
                      <ChevronDown className="h-3 w-3" /> Chats & Details
                    </>
                  )}
                </button>
                {expandedBots.has(bot.id) && (
                  <div className="px-4 pb-4 space-y-3 border-t border-white/[0.04]">
                    {/* Bot details */}
                    <div className="grid grid-cols-2 gap-4 pt-3">
                      <div>
                        <p className="text-[10px] text-white/30 mb-1">Bot ID</p>
                        <p className="text-xs text-white/60 font-mono">{bot.id}</p>
                      </div>
                      <div>
                        <p className="text-[10px] text-white/30 mb-1">Backend</p>
                        <p className="text-xs text-white/60">
                          {BACKEND_LABELS[bot.default_backend || 'claudecode'] || bot.default_backend || 'Claude Code'}
                          {bot.default_model_override ? ` / ${bot.default_model_override}` : ''}
                          {bot.default_model_effort ? ` (${bot.default_model_effort})` : ''}
                        </p>
                      </div>
                      <div>
                        <p className="text-[10px] text-white/30 mb-1">Allowed Chat IDs</p>
                        <p className="text-xs text-white/60">
                          {bot.allowed_chat_ids?.length
                            ? bot.allowed_chat_ids.join(', ')
                            : 'All chats'}
                        </p>
                      </div>
                      <div>
                        <p className="text-[10px] text-white/30 mb-1">Created</p>
                        <p className="text-xs text-white/60">
                          {new Date(bot.created_at).toLocaleString()}
                        </p>
                      </div>
                    </div>
                    {bot.instructions && (
                      <div>
                        <p className="text-[10px] text-white/30 mb-1">Instructions</p>
                        <p className="text-xs text-white/60 whitespace-pre-wrap bg-white/[0.02] rounded-lg p-2 border border-white/[0.04]">
                          {bot.instructions}
                        </p>
                      </div>
                    )}

                    {/* Chat-to-mission mappings */}
                    <div>
                      <p className="text-[10px] text-white/30 mb-2">Active Conversations</p>
                      {loadingChats.has(bot.id) ? (
                        <div className="flex items-center gap-2 text-xs text-white/40">
                          <Loader className="h-3 w-3 animate-spin" /> Loading...
                        </div>
                      ) : (chatsByBot[bot.id] || []).length === 0 ? (
                        <p className="text-xs text-white/30 italic">
                          No conversations yet. Message the bot on Telegram to start one.
                        </p>
                      ) : (
                        <div className="space-y-1">
                          {(chatsByBot[bot.id] || []).map((chat) => (
                            <div
                              key={chat.id}
                              className="flex items-center gap-3 px-3 py-2 rounded-lg bg-white/[0.02] border border-white/[0.04]"
                            >
                              <div className="flex-1 min-w-0">
                                <p className="text-xs text-white/60">
                                  Chat {chat.chat_id}
                                  {chat.chat_title && (
                                    <span className="text-white/40"> ({chat.chat_title})</span>
                                  )}
                                </p>
                                <p className="text-[10px] text-white/30">
                                  Mission: {getMissionTitle(chat.mission_id)}
                                </p>
                              </div>
                              <p className="text-[10px] text-white/20 shrink-0">
                                {new Date(chat.created_at).toLocaleDateString()}
                              </p>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>

                    <div>
                      <p className="text-[10px] text-white/30 mb-2">Recent Telegram Actions</p>
                      {loadingActions.has(bot.id) ? (
                        <div className="flex items-center gap-2 text-xs text-white/40">
                          <Loader className="h-3 w-3 animate-spin" /> Loading...
                        </div>
                      ) : (actionsByBot[bot.id] || []).length === 0 ? (
                        <p className="text-xs text-white/30 italic">
                          No native Telegram actions recorded yet.
                        </p>
                      ) : (
                        <div className="space-y-1">
                          {(actionsByBot[bot.id] || []).map((action) => (
                            <div
                              key={action.id}
                              className="px-3 py-2 rounded-lg bg-white/[0.02] border border-white/[0.04]"
                            >
                              <div className="flex items-center justify-between gap-3">
                                <div className="flex items-center gap-2 min-w-0">
                                  <span className="inline-flex items-center rounded bg-sky-500/10 border border-sky-500/20 px-1.5 py-0.5 text-[10px] font-medium text-sky-300">
                                    {action.action_kind}
                                  </span>
                                  <p className="text-xs text-white/60 truncate">
                                    {action.target_chat_title || `Chat ${action.target_chat_id}`}
                                    {action.delay_seconds > 0 ? ` · +${action.delay_seconds}s` : ''}
                                  </p>
                                </div>
                                <span className={cn(
                                  'inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium',
                                  action.status === 'sent'
                                    ? 'bg-emerald-500/10 text-emerald-400'
                                    : action.status === 'failed'
                                      ? 'bg-red-500/10 text-red-400'
                                      : 'bg-amber-500/10 text-amber-300'
                                )}>
                                  {action.status}
                                </span>
                              </div>
                              <p className="text-[11px] text-white/40 mt-1 line-clamp-2">{action.text}</p>
                              <p className="text-[10px] text-white/25 mt-1">
                                Target: {action.target_kind} {action.target_value}
                                {action.last_error ? ` · ${action.last_error}` : ''}
                              </p>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>

                    <div>
                      <p className="text-[10px] text-white/30 mb-2">Scheduled Telegram Actions</p>
                      {loadingScheduled.has(bot.id) ? (
                        <div className="flex items-center gap-2 text-xs text-white/40">
                          <Loader className="h-3 w-3 animate-spin" /> Loading...
                        </div>
                      ) : (scheduledByBot[bot.id] || []).length === 0 ? (
                        <p className="text-xs text-white/30 italic">
                          No scheduled Telegram messages for this bot.
                        </p>
                      ) : (
                        <div className="space-y-1">
                          {(scheduledByBot[bot.id] || []).map((message) => (
                            <div
                              key={message.id}
                              className="px-3 py-2 rounded-lg bg-white/[0.02] border border-white/[0.04]"
                            >
                              <div className="flex items-center justify-between gap-3">
                                <p className="text-xs text-white/60 truncate">
                                  {message.chat_title || `Chat ${message.chat_id}`}
                                </p>
                                <span className={cn(
                                  'inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium',
                                  message.status === 'sent'
                                    ? 'bg-emerald-500/10 text-emerald-400'
                                    : message.status === 'failed'
                                      ? 'bg-red-500/10 text-red-400'
                                      : 'bg-amber-500/10 text-amber-300'
                                )}>
                                  {message.status}
                                </span>
                              </div>
                              <p className="text-[11px] text-white/40 mt-1 line-clamp-2">{message.text}</p>
                              <p className="text-[10px] text-white/25 mt-1">
                                Send at {new Date(message.send_at).toLocaleString()}
                                {message.sent_at ? ` · Sent ${new Date(message.sent_at).toLocaleString()}` : ''}
                              </p>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>

                    <div>
                      <p className="text-[10px] text-white/30 mb-2">Structured Memory</p>
                      <div className="flex items-center gap-2 mb-3">
                        <input
                          type="text"
                          placeholder="Search structured memory..."
                          value={memorySearchQueryByBot[bot.id] || ''}
                          onChange={(e) =>
                            setMemorySearchQueryByBot((prev) => ({
                              ...prev,
                              [bot.id]: e.target.value,
                            }))
                          }
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') {
                              void loadMemorySearch(bot.id);
                            }
                          }}
                          className="flex-1 px-3 py-2 rounded-lg bg-white/[0.03] border border-white/[0.06] text-xs text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50"
                        />
                        <button
                          type="button"
                          onClick={() => void loadMemorySearch(bot.id)}
                          className="px-3 py-2 rounded-lg border border-white/[0.08] bg-white/[0.03] text-xs text-white/70 hover:text-white hover:border-white/[0.16] transition-colors"
                        >
                          Search
                        </button>
                      </div>
                      {loadingMemorySearch.has(bot.id) ? (
                        <div className="flex items-center gap-2 text-xs text-white/40 mb-3">
                          <Loader className="h-3 w-3 animate-spin" /> Searching...
                        </div>
                      ) : (bot.id in memorySearchByBot) && (memorySearchByBot[bot.id] || []).length > 0 ? (
                        <div className="space-y-1 mb-3">
                          {(memorySearchByBot[bot.id] || []).map((hit) => (
                            <div
                              key={`${bot.id}-${hit.entry.id}-search`}
                              className="px-3 py-2 rounded-lg bg-indigo-500/5 border border-indigo-500/15"
                            >
                              <div className="flex items-center justify-between gap-3">
                                <p className="text-xs text-indigo-200 truncate">
                                  {hit.entry.label ? `${hit.entry.label} · ` : ''}{hit.entry.value}
                                </p>
                                <span className="text-[10px] text-indigo-200/70 shrink-0">
                                  {hit.score.toFixed(1)}
                                </span>
                              </div>
                              <p className="text-[10px] text-indigo-100/50 mt-1">
                                {hit.reasons.join(' · ')}
                                {hit.matched_terms.length > 0 ? ` · terms: ${hit.matched_terms.join(', ')}` : ''}
                              </p>
                            </div>
                          ))}
                        </div>
                      ) : (bot.id in memorySearchByBot) && memorySearchQueryByBot[bot.id]?.trim() ? (
                        <p className="text-xs text-white/30 italic mb-3">
                          No ranked memory matches for this query.
                        </p>
                      ) : null}
                      {loadingMemory.has(bot.id) ? (
                        <div className="flex items-center gap-2 text-xs text-white/40">
                          <Loader className="h-3 w-3 animate-spin" /> Loading...
                        </div>
                      ) : (bot.id in memoryByBot) && (memoryByBot[bot.id] || []).length === 0 ? (
                        <p className="text-xs text-white/30 italic">
                          No structured memory captured yet.
                        </p>
                      ) : (
                        <div className="space-y-1">
                          {(memoryByBot[bot.id] || []).map((entry) => (
                            <div
                              key={entry.id}
                              className="px-3 py-2 rounded-lg bg-white/[0.02] border border-white/[0.04]"
                            >
                              <div className="flex items-center justify-between gap-3">
                                <div className="flex items-center gap-2 min-w-0">
                                  <span className="inline-flex items-center rounded bg-indigo-500/10 border border-indigo-500/20 px-1.5 py-0.5 text-[10px] font-medium text-indigo-300">
                                    {entry.kind}
                                  </span>
                                  <span className="inline-flex items-center rounded bg-white/[0.04] border border-white/[0.06] px-1.5 py-0.5 text-[10px] font-medium text-white/50">
                                    {entry.scope}
                                  </span>
                                  <p className="text-xs text-white/60 truncate">
                                    {entry.scope === 'user'
                                      ? entry.subject_display_name || entry.subject_username || `User ${entry.subject_user_id}`
                                      : `Chat ${entry.chat_id}`}
                                    {entry.label ? ` · ${entry.label}` : ''}
                                  </p>
                                </div>
                                <p className="text-[10px] text-white/20 shrink-0">
                                  {new Date(entry.updated_at).toLocaleDateString()}
                                </p>
                              </div>
                              <p className="text-[11px] text-white/40 mt-1 line-clamp-2">
                                {entry.value}
                              </p>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}

        {/* Info card */}
        <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6">
          <h3 className="text-base font-medium text-white mb-3">How it works</h3>
          <ol className="space-y-2 text-sm text-white/60 list-decimal list-inside">
            <li>Create a bot via <span className="text-white/80">@BotFather</span> on Telegram</li>
            <li>Add the bot here with a token and default mission settings</li>
            <li>Each new Telegram chat auto-creates a mission with your defaults</li>
            <li>Messages are routed to the chat&apos;s mission, responses streamed back</li>
          </ol>
          <p className="text-xs text-white/40 mt-4">
            For group chats, disable bot privacy mode via @BotFather (<code className="bg-white/[0.06] px-1 py-0.5 rounded">/setprivacy</code>) to let the bot see all messages.
          </p>
        </div>
      </div>

      {/* Create Dialog */}
      {showCreateDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-lg max-h-[90vh] overflow-y-auto p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Add Telegram Bot</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1">Bot Token</label>
                <input
                  type="password"
                  placeholder="123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
                  value={createBotToken}
                  onChange={(e) => setCreateBotToken(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50 font-mono text-sm"
                />
                <p className="text-[10px] text-white/30 mt-1">
                  Get this from @BotFather on Telegram
                </p>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Bot Username (optional)</label>
                <input
                  type="text"
                  placeholder="my_bot"
                  value={createBotUsername}
                  onChange={(e) => setCreateBotUsername(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50"
                />
                <p className="text-[10px] text-white/30 mt-1">
                  Auto-detected from token if omitted
                </p>
              </div>

              {/* Divider */}
              <div className="border-t border-white/[0.06] pt-4">
                <p className="text-xs text-white/40 mb-3">Default mission settings for new conversations</p>
              </div>

              <div>
                <label className="block text-sm text-white/60 mb-1">Backend</label>
                <select
                  value={createBackend}
                  onChange={(e) => setCreateBackend(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  {backends.length > 0
                    ? backends.map((b: Backend) => (
                        <option key={b.id} value={b.id}>
                          {BACKEND_LABELS[b.id] || b.name || b.id}
                        </option>
                      ))
                    : ['claudecode', 'opencode', 'codex', 'gemini', 'grok'].map((id) => (
                        <option key={id} value={id}>
                          {BACKEND_LABELS[id] || id}
                        </option>
                      ))}
                </select>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Model Override (optional)</label>
                <select
                  value={createModelOverride}
                  onChange={(e) => setCreateModelOverride(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50 text-sm [&>option]:bg-slate-800 [&>option]:text-white [&>optgroup]:bg-slate-900 [&>optgroup]:text-white/70"
                >
                  <option value="">No override (use default)</option>
                  {(() => {
                    const groupedOptions = new Map<string, Array<{ value: string; label: string; description?: string }>>();
                    for (const option of modelOptions) {
                      const providerName = option.label.split(' — ')[0] || 'Other';
                      if (!groupedOptions.has(providerName)) groupedOptions.set(providerName, []);
                      groupedOptions.get(providerName)!.push(option);
                    }
                    return Array.from(groupedOptions.entries()).map(([providerName, options]) => (
                      <optgroup key={providerName} label={providerName}>
                        {options.map((option) => {
                          const modelName = option.label.split(' — ')[1] || option.label;
                          const displayText = option.description ? `${modelName} - ${option.description}` : modelName;
                          return (
                            <option key={option.value} value={option.value}>{displayText}</option>
                          );
                        })}
                      </optgroup>
                    ));
                  })()}
                </select>
              </div>
              {(createBackend === 'claudecode' || createBackend === 'codex') && (
                <div>
                  <label className="block text-sm text-white/60 mb-1">Model Effort (optional)</label>
                  <select
                    value={createModelEffort}
                    onChange={(e) => setCreateModelEffort(e.target.value)}
                    className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                  >
                    <option value="">Default</option>
                    <option value="low">Low</option>
                    <option value="medium">Medium</option>
                    <option value="high">High</option>
                    {createBackend === 'claudecode' && <option value="xhigh">XHigh</option>}
                    {createBackend === 'claudecode' && <option value="max">Max</option>}
                  </select>
                </div>
              )}
              {workspaces.length > 0 && (
                <div>
                  <label className="block text-sm text-white/60 mb-1">Workspace (optional)</label>
                  <select
                    value={createWorkspaceId}
                    onChange={(e) => setCreateWorkspaceId(e.target.value)}
                    className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                  >
                    <option value="">Host (default)</option>
                    {workspaces.map((w: Workspace) => (
                      <option key={w.id} value={w.id}>
                        {w.name || w.id.slice(0, 8) + '...'}
                      </option>
                    ))}
                  </select>
                </div>
              )}
              <div>
                <label className="block text-sm text-white/60 mb-1">Config Profile (optional)</label>
                <select
                  value={createConfigProfile}
                  onChange={(e) => setCreateConfigProfile(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="">None (use workspace default)</option>
                  {configProfiles.map((p: ConfigProfileSummary) => (
                    <option key={p.name} value={p.name}>
                      {p.name}{p.is_default ? ' (default)' : ''}
                    </option>
                  ))}
                </select>
              </div>

              {/* Divider */}
              <div className="border-t border-white/[0.06] pt-4">
                <p className="text-xs text-white/40 mb-3">Bot behavior</p>
              </div>

              <div>
                <label className="block text-sm text-white/60 mb-1">Trigger Mode</label>
                <select
                  value={createTriggerMode}
                  onChange={(e) => setCreateTriggerMode(e.target.value as TelegramTriggerMode)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  {Object.entries(TRIGGER_MODE_LABELS).map(([mode, label]) => (
                    <option key={mode} value={mode}>
                      {label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Instructions (optional)</label>
                <textarea
                  placeholder="You are a helpful assistant. Respond in plain text without markdown."
                  value={createInstructions}
                  onChange={(e) => setCreateInstructions(e.target.value)}
                  rows={3}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50 resize-none text-sm"
                />
                <p className="text-[10px] text-white/30 mt-1">
                  Prepended to every message. Set personality and formatting rules.
                </p>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Allowed Chat IDs (optional)</label>
                <input
                  type="text"
                  placeholder="-1001234567890, 987654321"
                  value={createAllowedChatIds}
                  onChange={(e) => setCreateAllowedChatIds(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50 font-mono text-sm"
                />
                <p className="text-[10px] text-white/30 mt-1">
                  Leave empty to allow all chats. Comma-separated Telegram chat IDs.
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => {
                  setShowCreateDialog(false);
                  resetCreateForm();
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCreate}
                disabled={!createBotToken.trim() || creating}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {creating ? 'Creating...' : 'Add Bot'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Edit Dialog */}
      {editingBot && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-lg max-h-[90vh] overflow-y-auto p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">
              Edit @{editingBot.bot_username || 'bot'}
            </h3>
            <div className="space-y-4">
              {/* Mission settings */}
              <div className="border-b border-white/[0.06] pb-3">
                <p className="text-xs text-white/40 mb-3">Default mission settings for new conversations</p>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Backend</label>
                <select
                  value={editBackend}
                  onChange={(e) => setEditBackend(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  {backends.length > 0
                    ? backends.map((b: Backend) => (
                        <option key={b.id} value={b.id}>
                          {BACKEND_LABELS[b.id] || b.name || b.id}
                        </option>
                      ))
                    : ['claudecode', 'opencode', 'codex', 'gemini', 'grok'].map((id) => (
                        <option key={id} value={id}>
                          {BACKEND_LABELS[id] || id}
                        </option>
                      ))}
                </select>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Model Override</label>
                <select
                  value={editModelOverride}
                  onChange={(e) => setEditModelOverride(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50 text-sm [&>option]:bg-slate-800 [&>option]:text-white [&>optgroup]:bg-slate-900 [&>optgroup]:text-white/70"
                >
                  <option value="">No override (use default)</option>
                  {(() => {
                    const groupedOptions = new Map<string, Array<{ value: string; label: string; description?: string }>>();
                    for (const option of editModelOptions) {
                      const providerName = option.label.split(' — ')[0] || 'Other';
                      if (!groupedOptions.has(providerName)) groupedOptions.set(providerName, []);
                      groupedOptions.get(providerName)!.push(option);
                    }
                    return Array.from(groupedOptions.entries()).map(([providerName, options]) => (
                      <optgroup key={providerName} label={providerName}>
                        {options.map((option) => {
                          const modelName = option.label.split(' — ')[1] || option.label;
                          const displayText = option.description ? `${modelName} - ${option.description}` : modelName;
                          return (
                            <option key={option.value} value={option.value}>{displayText}</option>
                          );
                        })}
                      </optgroup>
                    ));
                  })()}
                </select>
              </div>
              {(editBackend === 'claudecode' || editBackend === 'codex') && (
                <div>
                  <label className="block text-sm text-white/60 mb-1">Model Effort</label>
                  <select
                    value={editModelEffort}
                    onChange={(e) => setEditModelEffort(e.target.value)}
                    className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                  >
                    <option value="">Default</option>
                    <option value="low">Low</option>
                    <option value="medium">Medium</option>
                    <option value="high">High</option>
                    {editBackend === 'claudecode' && <option value="xhigh">XHigh</option>}
                    {editBackend === 'claudecode' && <option value="max">Max</option>}
                  </select>
                </div>
              )}
              {workspaces.length > 0 && (
                <div>
                  <label className="block text-sm text-white/60 mb-1">Workspace</label>
                  <select
                    value={editWorkspaceId}
                    onChange={(e) => setEditWorkspaceId(e.target.value)}
                    className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                  >
                    <option value="">Host (default)</option>
                    {workspaces.map((w: Workspace) => (
                      <option key={w.id} value={w.id}>
                        {w.name || w.id.slice(0, 8) + '...'}
                      </option>
                    ))}
                  </select>
                </div>
              )}
              <div>
                <label className="block text-sm text-white/60 mb-1">Config Profile</label>
                <select
                  value={editConfigProfile}
                  onChange={(e) => setEditConfigProfile(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="">None (use workspace default)</option>
                  {configProfiles.map((p: ConfigProfileSummary) => (
                    <option key={p.name} value={p.name}>
                      {p.name}{p.is_default ? ' (default)' : ''}
                    </option>
                  ))}
                </select>
              </div>

              {/* Bot behavior */}
              <div className="border-t border-white/[0.06] pt-4">
                <p className="text-xs text-white/40 mb-3">Bot behavior</p>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Trigger Mode</label>
                <select
                  value={editTriggerMode}
                  onChange={(e) => setEditTriggerMode(e.target.value as TelegramTriggerMode)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  {Object.entries(TRIGGER_MODE_LABELS).map(([mode, label]) => (
                    <option key={mode} value={mode}>
                      {label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Instructions</label>
                <textarea
                  placeholder="System instructions for this assistant..."
                  value={editInstructions}
                  onChange={(e) => setEditInstructions(e.target.value)}
                  rows={4}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50 resize-none text-sm"
                />
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => setEditingBot(null)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveEdit}
                disabled={saving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {saving ? 'Saving...' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
