'use client';

import { useState, useEffect, useMemo, useCallback } from 'react';
import useSWR from 'swr';
import {
  listAssistantGateways,
  createAssistantGateway,
  updateAssistantGateway,
  deleteAssistantGateway,
  listAssistantGatewayChats,
  listAssistantGatewayActions,
  listAssistantGatewayScheduledMessages,
  listAssistantGatewayMemory,
  searchAssistantGatewayMemory,
  adoptHermesAssistant,
  sendAssistantGatewayTestMessage,
  getHermesAssistantStatus,
  listMissions,
  getSystemComponents,
  type Mission,
  type AssistantGateway,
  type AssistantGatewayActionExecution,
  type AssistantGatewayChat,
  type AssistantGatewayScheduledMessage,
  type AssistantGatewayMemoryEntry,
  type AssistantGatewayMemorySearchHit,
  type TelegramTriggerMode,
  type CreateAssistantGatewayInput,
} from '@/lib/api';
import { listBackends, listWorkspaces, listBackendModelOptions, listProviders, listConfigProfiles, type Backend, type BackendModelOption, type Provider, type Workspace, type ConfigProfileSummary } from '@/lib/api';
import { listMissionAutomations, listHermesAssistantSkills, type Automation, type CommandSource, type TriggerType, type HermesSkill, type HermesSkillsResponse } from '@/lib/api';
import {
  MessageCircle,
  Plus,
  Trash2,
  Loader,
  Power,
  PowerOff,
  Bot,
  Cable,
  ChevronDown,
  ChevronUp,
  CheckCircle2,
  CircleDashed,
  Settings,
  AlertTriangle,
  GitBranch,
  Send,
  Search,
  Calendar,
  Sparkles,
  ListChecks,
  User,
  Activity,
  Brain,
  Globe,
  Copy,
  Check,
  RefreshCw,
  Eye,
  EyeOff,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast } from '@/components/toast';
import { listModelChains, type ModelChain } from '@/lib/api/model-routing';
import { getHermesRemoteStatus, rotateHermesRemoteKey, applyHermesRemote, type HermesRemoteStatus } from '@/lib/api/assistant';
import { getRuntimeApiBase } from '@/lib/settings';

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
  chatgpt_ui: 'ChatGPT UI (experimental)',
};

function gatewayLabel(bot: AssistantGateway) {
  return bot.bot_username ? `@${bot.bot_username}` : 'gateway';
}

type GatewayTab = 'conversations' | 'actions' | 'scheduled' | 'tasks' | 'memory' | 'skills';

const GATEWAY_TABS: { id: GatewayTab; label: string }[] = [
  { id: 'conversations', label: 'Conversations' },
  { id: 'actions', label: 'Actions' },
  { id: 'scheduled', label: 'Scheduled' },
  { id: 'tasks', label: 'Tasks' },
  { id: 'memory', label: 'Memory' },
  { id: 'skills', label: 'Skills' },
];

// Solid, saturated status dots read cleanly on both light and dark surfaces.
const STATUS_DOT: Record<string, string> = {
  sent: 'bg-emerald-400',
  pending: 'bg-amber-300',
  failed: 'bg-red-400',
};

function relTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return '';
  const s = Math.round((Date.now() - then) / 1000);
  if (s < 45) return 'just now';
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 30) return `${d}d ago`;
  const mo = Math.round(d / 30);
  if (mo < 12) return `${mo}mo ago`;
  return `${Math.round(mo / 12)}y ago`;
}

// Shared row chrome: hairline-separated dense rows that theme in both modes via
// the globals.css light remap (border-white/* and hover:bg-white/* are remapped).
const ROW = 'group border-t border-white/[0.04] first:border-t-0';
const ROW_PAD = 'flex items-start gap-3 px-1 py-2';

function humanizeDuration(seconds: number): string {
  if (seconds <= 0) return '0s';
  if (seconds % 86400 === 0) return `${seconds / 86400}d`;
  if (seconds % 3600 === 0) return `${seconds / 3600}h`;
  if (seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

function triggerLabel(t: TriggerType): string {
  switch (t.type) {
    case 'interval':
      return `every ${humanizeDuration(t.seconds)}`;
    case 'agent_finished':
      return 'when agent finishes';
    case 'durable_job_terminal':
      return 'when durable job finishes';
    case 'cron':
      return `cron ${t.expression}`;
    case 'telegram':
      return 'telegram';
    case 'webhook':
      return 'webhook';
    default:
      return String((t as { type: string }).type).replace(/_/g, ' ');
  }
}

function automationLabel(cs: CommandSource): string {
  switch (cs.type) {
    case 'library':
      return cs.name;
    case 'local_file':
      return cs.path.split('/').pop() || cs.path;
    case 'inline':
      return cs.content.split('\n')[0].slice(0, 80) || 'inline command';
    case 'native_loop':
      return `/${cs.command}${cs.harness ? ` · ${cs.harness}` : ''}`;
    default:
      return 'command';
  }
}

// A small empty-state used for populated lists and genuinely-empty tabs.
function EmptyHint({ icon: Icon, children }: { icon: typeof Sparkles; children: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-4 py-8 text-center">
      <Icon className="h-5 w-5 text-white/20" />
      <p className="text-xs text-white/35 max-w-xs">{children}</p>
    </div>
  );
}

export default function AssistantPage() {
  const { data: bots = [], mutate: mutateBots, isLoading: botsLoading } = useSWR(
    'assistant-gateways',
    listAssistantGateways,
    { revalidateOnFocus: false }
  );
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
  const { data: systemComponents, mutate: mutateSystemComponents, isLoading: componentsLoading } = useSWR(
    'system-components',
    getSystemComponents,
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );
  const { data: configProfiles = [] } = useSWR('config-profiles', listConfigProfiles, {
    revalidateOnFocus: false,
  });
  const { data: modelChains = [] } = useSWR<ModelChain[]>('model-chains', listModelChains, {
    revalidateOnFocus: false,
    dedupingInterval: 60000,
  });
  const { data: hermesStatus, mutate: mutateHermesStatus } = useSWR(
    'hermes-assistant-status',
    getHermesAssistantStatus,
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );
  const { data: hermesSkillsData, isLoading: skillsLoading } = useSWR<HermesSkillsResponse>(
    'hermes-skills',
    listHermesAssistantSkills,
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );
  const { data: hermesRemote, mutate: mutateHermesRemote } = useSWR<HermesRemoteStatus>(
    'hermes-remote-status',
    getHermesRemoteStatus,
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );
  const [rotatingRemoteKey, setRotatingRemoteKey] = useState(false);
  const [applyingRemote, setApplyingRemote] = useState(false);
  const [remoteKeyVisible, setRemoteKeyVisible] = useState(false);
  const [copiedRemoteText, setCopiedRemoteText] = useState<string | null>(null);
  const hermesRemoteUrl = `${getRuntimeApiBase()}${hermesRemote?.path ?? '/hermes-remote'}`;
  // Key provisioned in the env but the API server isn't answering yet —
  // a gateway restart (Apply) brings it up.
  const hermesRemotePending = Boolean(
    hermesRemote?.installed && hermesRemote?.key && !hermesRemote?.active
  );

  const copyRemoteText = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedRemoteText(text);
      setTimeout(() => setCopiedRemoteText(null), 2000);
    } catch {
      // Ignore.
    }
  };

  const handleRotateRemoteKey = async () => {
    if (
      hermesRemote?.key &&
      !confirm(
        'Generate a new remote token? The previous token stops working and the Hermes gateway restarts.'
      )
    ) {
      return;
    }
    setRotatingRemoteKey(true);
    try {
      await rotateHermesRemoteKey();
      mutateHermesRemote();
    } catch (e) {
      alert(e instanceof Error ? e.message : 'Failed to generate token');
    } finally {
      setRotatingRemoteKey(false);
    }
  };

  const handleApplyRemote = async () => {
    if (!confirm('Restart the Hermes gateway to activate remote access?')) return;
    setApplyingRemote(true);
    try {
      await applyHermesRemote();
      mutateHermesRemote();
    } catch (e) {
      alert(e instanceof Error ? e.message : 'Failed to apply remote settings');
    } finally {
      setApplyingRemote(false);
    }
  };
  const hermesSkills = hermesSkillsData?.skills ?? [];

  // Chat mappings keyed by bot ID
  const [chatsByBot, setChatsByBot] = useState<Record<string, AssistantGatewayChat[]>>({});
  const [actionsByBot, setActionsByBot] = useState<Record<string, AssistantGatewayActionExecution[]>>({});
  const [scheduledByBot, setScheduledByBot] = useState<Record<string, AssistantGatewayScheduledMessage[]>>({});
  const [memoryByBot, setMemoryByBot] = useState<Record<string, AssistantGatewayMemoryEntry[]>>({});
  const [automationsByBot, setAutomationsByBot] = useState<Record<string, Automation[]>>({});
  const [loadingAutomations, setLoadingAutomations] = useState<Set<string>>(new Set());
  const [memorySearchByBot, setMemorySearchByBot] = useState<Record<string, AssistantGatewayMemorySearchHit[]>>({});
  const [memorySearchQueryByBot, setMemorySearchQueryByBot] = useState<Record<string, string>>({});
  const [expandedBots, setExpandedBots] = useState<Set<string>>(new Set());
  const [loadingChats, setLoadingChats] = useState<Set<string>>(new Set());
  const [loadingActions, setLoadingActions] = useState<Set<string>>(new Set());
  const [loadingScheduled, setLoadingScheduled] = useState<Set<string>>(new Set());
  const [loadingMemory, setLoadingMemory] = useState<Set<string>>(new Set());
  const [loadingMemorySearch, setLoadingMemorySearch] = useState<Set<string>>(new Set());
  const [adoptingGatewayId, setAdoptingGatewayId] = useState<string | null>(null);

  // Per-gateway active tab, memory sub-view, expanded detail rows, and in-flight tests.
  const [activeTabByBot, setActiveTabByBot] = useState<Record<string, GatewayTab>>({});
  const [memoryViewByBot, setMemoryViewByBot] = useState<Record<string, 'entries' | 'profiles'>>({});
  const [conversationQueryByBot, setConversationQueryByBot] = useState<Record<string, string>>({});
  const [expandedRows, setExpandedRows] = useState<Set<string>>(new Set());
  const [sendingTest, setSendingTest] = useState<Set<string>>(new Set());

  const toggleRow = (key: string) =>
    setExpandedRows((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

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
  const getModelOptionsForBackend = useCallback((backend: string) => {
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
        options.push({ value, label: `${provider.name} · ${model.name}`, description: model.description });
      }
    }
    return options;
  }, [backendModelOptions, providersResponse]);

  const modelOptions = useMemo(() => getModelOptionsForBackend(createBackend), [getModelOptionsForBackend, createBackend]);

  // Edit dialog
  const [editingBot, setEditingBot] = useState<AssistantGateway | null>(null);
  const [editInstructions, setEditInstructions] = useState('');
  const [editTriggerMode, setEditTriggerMode] = useState<TelegramTriggerMode>('mention_or_dm');
  const [editBackend, setEditBackend] = useState('');
  const [editModelOverride, setEditModelOverride] = useState('');
  const [editModelEffort, setEditModelEffort] = useState('');
  const [editWorkspaceId, setEditWorkspaceId] = useState('');
  const [editConfigProfile, setEditConfigProfile] = useState('');
  const [saving, setSaving] = useState(false);

  const editModelOptions = useMemo(() => getModelOptionsForBackend(editBackend || 'claudecode'), [getModelOptionsForBackend, editBackend]);

  const loadChats = async (botId: string) => {
    if (chatsByBot[botId]) return; // already loaded
    setLoadingChats((prev) => new Set(prev).add(botId));
    try {
      const chats = await listAssistantGatewayChats(botId);
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

  // Tasks = scheduled automations on the missions this gateway drives. The
  // gateway↔mission link comes from its chats, so resolve those first.
  const loadAutomations = async (botId: string) => {
    if (automationsByBot[botId]) return;
    setLoadingAutomations((prev) => new Set(prev).add(botId));
    try {
      let chats = chatsByBot[botId];
      if (!chats) {
        chats = await listAssistantGatewayChats(botId);
        setChatsByBot((prev) => ({ ...prev, [botId]: chats! }));
      }
      const missionIds = Array.from(new Set(chats.map((c) => c.mission_id)));
      const lists = await Promise.all(
        missionIds.map((id) => listMissionAutomations(id).catch(() => [] as Automation[]))
      );
      const seen = new Set<string>();
      const deduped = lists.flat().filter((a) => (seen.has(a.id) ? false : (seen.add(a.id), true)));
      setAutomationsByBot((prev) => ({ ...prev, [botId]: deduped }));
    } catch {
      // ignore
    } finally {
      setLoadingAutomations((prev) => {
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
      const scheduled = await listAssistantGatewayScheduledMessages(botId, { limit: 20 });
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
      const actions = await listAssistantGatewayActions(botId, { limit: 20 });
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
      const memory = await listAssistantGatewayMemory(botId, { limit: 50 });
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
      const hits = await searchAssistantGatewayMemory(botId, { q: query, limit: 6 });
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
      void loadAutomations(botId);
    }
  };

  const handleCreate = async () => {
    if (!createBotToken.trim()) return;
    setCreating(true);
    try {
      const input: CreateAssistantGatewayInput = {
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

      const bot = await createAssistantGateway(input);
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

  const handleToggleActive = async (bot: AssistantGateway) => {
    try {
      await updateAssistantGateway(bot.id, { active: !bot.active });
      await mutateBots();
      toast.success(bot.active ? 'Bot deactivated' : 'Bot activated');
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to toggle bot');
    }
  };

  const handleDelete = async (bot: AssistantGateway) => {
    if (!confirm(`Delete bot @${bot.bot_username || bot.id.slice(0, 8)}?`)) return;
    try {
      await deleteAssistantGateway(bot.id);
      await mutateBots();
      toast.success('Bot deleted');
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to delete bot');
    }
  };

  const handleAdoptHermes = async (bot: AssistantGateway) => {
    const allowAllUsers = !bot.allowed_chat_ids?.length;
    const label = gatewayLabel(bot);
    const warning = allowAllUsers
      ? `${label} does not have allowed user IDs configured. Adopt it into Hermes with open Telegram access?`
      : `Move ${label} from the compatibility webhook to Hermes?`;
    if (!confirm(warning)) return;

    setAdoptingGatewayId(bot.id);
    try {
      const result = await adoptHermesAssistant({
        gateway_id: bot.id,
        allow_all_users: allowAllUsers,
        model: assistantChain?.id || 'builtin/smart',
        install_hermes_if_missing: true,
      });
      await Promise.all([mutateBots(), mutateSystemComponents(), mutateHermesStatus()]);
      toast.success(
        result.ok
          ? `${label} is now managed by ${result.service_name}`
          : `${label} was adopted, but Hermes reported ${result.hermes_status}`
      );
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to adopt gateway into Hermes');
      await Promise.all([mutateBots(), mutateSystemComponents(), mutateHermesStatus()]);
    } finally {
      setAdoptingGatewayId(null);
    }
  };

  const handleSendTest = async (bot: AssistantGateway, chat: AssistantGatewayChat) => {
    const key = `${bot.id}:${chat.chat_id}`;
    setSendingTest((prev) => new Set(prev).add(key));
    try {
      await sendAssistantGatewayTestMessage(bot.id, chat.chat_id);
      toast.success(`Test message sent to ${chat.chat_title || `chat ${chat.chat_id}`}`);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to send test message');
    } finally {
      setSendingTest((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };

  const handleSaveEdit = async () => {
    if (!editingBot) return;
    setSaving(true);
    try {
      await updateAssistantGateway(editingBot.id, {
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

  // Index missions by ID once per render so per-chat lookups stay O(1).
  const missionsById = useMemo(() => {
    const map = new Map<string, Mission>();
    for (const m of missions) map.set(m.id, m);
    return map;
  }, [missions]);
  const getMissionTitle = useCallback(
    (missionId: string) => {
      const m = missionsById.get(missionId);
      return m?.title || missionId.slice(0, 8) + '...';
    },
    [missionsById]
  );
  const activeGatewayCount = useMemo(
    () => bots.filter((bot) => bot.active).length,
    [bots]
  );
  const knownConversationCount = useMemo(
    () => Object.values(chatsByBot).reduce((count, chats) => count + chats.length, 0),
    [chatsByBot]
  );
  const assistantMcp = useMemo(
    () => systemComponents?.components.find((component) => component.name === 'assistant_mcp'),
    [systemComponents]
  );
  const assistantMcpReady = assistantMcp?.installed && assistantMcp.status === 'ok';
  const hermesRuntime = useMemo(
    () => systemComponents?.components.find((component) => component.name === 'hermes_assistant'),
    [systemComponents]
  );
  const hermesRuntimeReady = hermesRuntime?.installed && hermesRuntime.status === 'ok';
  const assistantChain = useMemo(
    () =>
      modelChains.find((chain) => chain.id === 'builtin/smart') ||
      modelChains.find((chain) => chain.id === 'builtin/assistant') ||
      modelChains.find((chain) => chain.id === 'assistant') ||
      modelChains.find((chain) => chain.is_default) ||
      null,
    [modelChains]
  );

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
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <div className="w-full min-w-[720px] max-w-5xl space-y-6 max-lg:min-w-0">
        {/* Header */}
        <header className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0">
            <h1 className="text-xl font-semibold text-white">Assistant</h1>
            <p className="mt-1 text-sm text-white/50">
              Hermes runtime, Telegram gateways, and assistant memory.
            </p>
          </div>
          <button
            onClick={() => setShowCreateDialog(true)}
            className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors flex-shrink-0"
          >
            <Plus className="h-3.5 w-3.5" />
            Add Gateway
          </button>
        </header>

        <div className="grid gap-3 md:grid-cols-4">
          <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs font-medium uppercase tracking-[0.08em] text-white/40">MCP</p>
              {assistantMcpReady ? (
                <CheckCircle2 className="h-4 w-4 text-emerald-400" />
              ) : (
                <CircleDashed className="h-4 w-4 text-amber-300" />
              )}
            </div>
            <p className="mt-2 text-sm font-medium text-white">
              {componentsLoading
                ? 'Checking assistant-mcp'
                : assistantMcpReady
                  ? `assistant-mcp ${assistantMcp.version || ''}`.trim()
                  : 'assistant-mcp not ready'}
            </p>
            <p className="mt-1 text-xs text-white/45">
              {assistantMcpReady
                ? `${assistantMcp.path || 'assistant-mcp'} is available for Hermes.`
                : 'Install assistant-mcp before handing mission control to Hermes.'}
            </p>
          </div>
          <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs font-medium uppercase tracking-[0.08em] text-white/40">Gateway</p>
              <Cable className="h-4 w-4 text-white/30" />
            </div>
            <p className="mt-2 text-sm font-medium text-white">
              {hermesStatus?.telegram_ok
                ? `@${hermesStatus.telegram_bot_username || 'telegram'} via Hermes`
                : `${activeGatewayCount} active / ${bots.length || 0} configured`}
            </p>
            <p className="mt-1 text-xs text-white/45">
              {hermesStatus?.telegram_ok
                ? hermesStatus.telegram_webhook_configured
                  ? 'Telegram webhook is still configured; polling may be blocked.'
                  : `Polling ready; ${hermesStatus.telegram_pending_update_count ?? 0} pending updates.`
                : `${knownConversationCount} known conversation${knownConversationCount === 1 ? '' : 's'}.`}
            </p>
          </div>
          <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs font-medium uppercase tracking-[0.08em] text-white/40">Runtime</p>
              {hermesRuntimeReady ? (
                <CheckCircle2 className="h-4 w-4 text-emerald-400" />
              ) : (
                <CircleDashed className="h-4 w-4 text-amber-300" />
              )}
            </div>
            <p className="mt-2 text-sm font-medium text-white">
              {componentsLoading
                ? 'Checking Hermes runtime'
                : hermesRuntimeReady
                  ? 'Hermes runtime active'
                  : hermesRuntime?.installed
                    ? 'Hermes runtime not healthy'
                    : 'Hermes runtime not installed'}
            </p>
            <p className="mt-1 text-xs text-white/45">
              {componentsLoading
                ? 'Checking the host runtime service.'
                : hermesRuntimeReady
                  ? `${hermesRuntime.path || 'hermes-assistant.service'} owns the assistant runtime.`
                  : hermesRuntime?.installed
                    ? `Service reported ${hermesRuntime.status || 'not healthy'}; keep Telegram in compatibility mode.`
                    : 'Install hermes-assistant-dev.service before moving webhook ownership.'}
            </p>
          </div>
          <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs font-medium uppercase tracking-[0.08em] text-white/40">Routing</p>
              <GitBranch className="h-4 w-4 text-white/30" />
            </div>
            <p className="mt-2 text-sm font-medium text-white">
              {hermesStatus?.model || assistantChain?.id || 'builtin/smart'}
            </p>
            <p className="mt-1 text-xs text-white/45">
              {assistantChain
                ? `${assistantChain.entries.length} fallback ${assistantChain.entries.length === 1 ? 'entry' : 'entries'} via /v1.`
                : 'Hermes should use the sandboxed.sh /v1 proxy chain.'}
            </p>
          </div>
        </div>

        {/* Remote access: connect the Hermes desktop app in proxy mode */}
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <Globe className="h-4 w-4 text-indigo-400" />
              <p className="text-sm font-medium text-white">Remote Access</p>
              {hermesRemote?.active ? (
                <span className="rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] font-medium text-emerald-300">
                  active
                </span>
              ) : hermesRemotePending ? (
                <span className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-300">
                  restart to activate
                </span>
              ) : (
                <span className="rounded bg-white/[0.06] px-1.5 py-0.5 text-[10px] font-medium text-white/40">
                  {hermesRemote?.installed ? 'inactive' : 'runtime not adopted'}
                </span>
              )}
            </div>
            <div className="flex items-center gap-2">
              {hermesRemotePending && (
                <button
                  onClick={handleApplyRemote}
                  disabled={applyingRemote}
                  className="flex items-center gap-1.5 rounded-lg border border-amber-500/25 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-300 transition-colors hover:bg-amber-500/20 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {applyingRemote ? (
                    <Loader className="h-3 w-3 animate-spin" />
                  ) : (
                    <Power className="h-3 w-3" />
                  )}
                  Apply (restart gateway)
                </button>
              )}
              <button
                onClick={handleRotateRemoteKey}
                disabled={rotatingRemoteKey || !hermesRemote?.installed}
                className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 transition-colors hover:bg-white/[0.04] disabled:cursor-not-allowed disabled:opacity-50"
                title={
                  hermesRemote?.installed
                    ? undefined
                    : 'Adopt the Hermes runtime first'
                }
              >
                {rotatingRemoteKey ? (
                  <Loader className="h-3 w-3 animate-spin" />
                ) : (
                  <RefreshCw className="h-3 w-3" />
                )}
                Rotate token
              </button>
            </div>
          </div>
          <p className="mt-1 text-xs text-white/45">
            Connect the Hermes desktop app in proxy mode: platform I/O stays local, agent work runs here.
            Set <code className="rounded bg-white/[0.06] px-1 font-mono text-[10px] text-indigo-300">GATEWAY_PROXY_URL</code> to
            the endpoint below and <code className="rounded bg-white/[0.06] px-1 font-mono text-[10px] text-indigo-300">GATEWAY_PROXY_KEY</code> to
            your token.
          </p>
          <div className="mt-3 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
            <div className="flex items-center justify-between">
              <div className="min-w-0">
                <span className="text-[10px] uppercase tracking-wider text-white/30">Hermes Remote Endpoint</span>
                <p className="mt-0.5 truncate font-mono text-xs text-white/60">{hermesRemoteUrl}</p>
              </div>
              <button
                onClick={() => copyRemoteText(hermesRemoteUrl)}
                className={cn(
                  'shrink-0 rounded-md p-1.5 transition-colors',
                  copiedRemoteText === hermesRemoteUrl
                    ? 'text-emerald-400'
                    : 'text-white/20 hover:bg-white/[0.04] hover:text-white/60'
                )}
                title="Copy endpoint URL"
              >
                {copiedRemoteText === hermesRemoteUrl ? (
                  <Check className="h-3.5 w-3.5" />
                ) : (
                  <Copy className="h-3.5 w-3.5" />
                )}
              </button>
            </div>
          </div>
          {hermesRemote?.key && (
            <div className="mt-2 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
              <div className="flex items-center justify-between">
                <div className="min-w-0 flex-1">
                  <span className="text-[10px] uppercase tracking-wider text-white/30">Token (GATEWAY_PROXY_KEY)</span>
                  <p className="mt-0.5 truncate font-mono text-xs text-white/60">
                    {remoteKeyVisible
                      ? hermesRemote.key
                      : `${hermesRemote.key.slice(0, 8)}${'•'.repeat(24)}`}
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <button
                    onClick={() => setRemoteKeyVisible((v) => !v)}
                    className="rounded-md p-1.5 text-white/20 transition-colors hover:bg-white/[0.04] hover:text-white/60"
                    title={remoteKeyVisible ? 'Hide token' : 'Reveal token'}
                  >
                    {remoteKeyVisible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                  </button>
                  <button
                    onClick={() => copyRemoteText(hermesRemote.key!)}
                    className={cn(
                      'rounded-md p-1.5 transition-colors',
                      copiedRemoteText === hermesRemote.key
                        ? 'text-emerald-400'
                        : 'text-white/20 hover:bg-white/[0.04] hover:text-white/60'
                    )}
                    title="Copy token"
                  >
                    {copiedRemoteText === hermesRemote.key ? (
                      <Check className="h-3.5 w-3.5" />
                    ) : (
                      <Copy className="h-3.5 w-3.5" />
                    )}
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>

        {hermesRuntimeReady && activeGatewayCount > 0 && (
          <div className="flex gap-3 rounded-lg border border-amber-500/20 bg-amber-500/[0.06] p-4">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-300" />
            <div className="min-w-0">
              <p className="text-sm font-medium text-white">Compatibility gateway still active</p>
              <p className="mt-1 text-xs leading-5 text-white/50">
                Hermes runtime is active while {activeGatewayCount} compatibility gateway{activeGatewayCount === 1 ? '' : 's'} {activeGatewayCount === 1 ? 'remains' : 'remain'} active.
                Use Adopt on the matching gateway to copy the existing token into Hermes and stop the legacy webhook.
              </p>
              <a
                href="#assistant-gateways"
                className="mt-3 inline-flex text-xs font-medium text-amber-200 hover:text-amber-100"
              >
                Review gateways
              </a>
            </div>
          </div>
        )}

        {hermesStatus?.telegram_last_error && (
          <div className="flex gap-3 rounded-lg border border-red-500/20 bg-red-500/[0.06] p-4">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-300" />
            <div className="min-w-0">
              <p className="text-sm font-medium text-white">Telegram gateway needs attention</p>
              <p className="mt-1 text-xs leading-5 text-white/50">
                {hermesStatus.telegram_last_error}
              </p>
            </div>
          </div>
        )}

        {/* Gateway list */}
        <section id="assistant-gateways" aria-label="Assistant gateways">
        {botsLoading ? (
          <div className="space-y-4" aria-busy="true" aria-label="Loading assistant gateways">
            {Array.from({ length: 2 }).map((_, i) => (
              <div
                key={i}
                className="rounded-xl border border-white/[0.06] bg-white/[0.02] overflow-hidden animate-pulse"
              >
                <div className="p-4 flex items-center gap-4">
                  <div className="h-10 w-10 rounded-lg bg-white/[0.04] flex-shrink-0" />
                  <div className="flex-1 min-w-0 space-y-2">
                    <div className="flex items-center gap-2">
                      <div className="h-4 w-32 rounded bg-white/[0.06]" />
                      <div className="h-4 w-14 rounded-full bg-white/[0.04]" />
                      <div className="h-4 w-20 rounded bg-white/[0.04]" />
                    </div>
                    <div className="h-3 w-40 rounded bg-white/[0.04]" />
                  </div>
                  <div className="flex items-center gap-1">
                    {Array.from({ length: 3 }).map((__, j) => (
                      <div key={j} className="h-7 w-7 rounded-lg bg-white/[0.04]" />
                    ))}
                  </div>
                </div>
                <div className="h-7 border-t border-white/[0.04] bg-white/[0.01]" />
              </div>
            ))}
          </div>
        ) : bots.length === 0 ? (
          <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-12 text-center">
            <MessageCircle className="h-12 w-12 text-white/20 mx-auto mb-4" />
            <h3 className="text-lg font-medium text-white mb-2">No assistant gateway</h3>
            <p className="text-sm text-white/50 mb-6 max-w-md mx-auto">
              Connect the current Telegram bridge while Hermes takes over assistant runtime and memory.
              Each chat can still create a mission during the cutover.
            </p>
            <button
              onClick={() => setShowCreateDialog(true)}
              className="inline-flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
            >
              <Plus className="h-4 w-4" />
              Add Gateway
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
                    <div className="flex flex-wrap items-center gap-2">
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
                      {hermesRuntimeReady && bot.active && (
                        <span className="inline-flex items-center rounded-full border border-amber-500/20 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-300">
                          Compatibility webhook
                        </span>
                      )}
                      <span className="inline-flex items-center rounded bg-white/[0.06] px-1.5 py-0.5 text-[10px] text-white/40">
                        {TRIGGER_MODE_LABELS[bot.trigger_mode]}
                      </span>
                      {bot.auto_create_missions && (
                        <span className="inline-flex items-center rounded bg-indigo-500/10 border border-indigo-500/20 px-1.5 py-0.5 text-[10px] font-medium text-indigo-400">
                          Auto-create
                        </span>
                      )}
                    </div>
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mt-0.5">
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
                    {assistantMcpReady && bot.active && (
                      <button
                        type="button"
                        onClick={() => void handleAdoptHermes(bot)}
                        disabled={adoptingGatewayId === bot.id}
                        className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-emerald-500/20 bg-emerald-500/10 px-2.5 text-xs font-medium text-emerald-300 transition-colors hover:border-emerald-500/30 hover:bg-emerald-500/15 disabled:cursor-not-allowed disabled:opacity-60"
                        aria-label={`Adopt ${gatewayLabel(bot)} into Hermes`}
                        title="Move this bot token and Telegram ownership to Hermes"
                      >
                        {adoptingGatewayId === bot.id ? (
                          <Loader className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <CheckCircle2 className="h-3.5 w-3.5" />
                        )}
                        Adopt
                      </button>
                    )}
                    <button
                      type="button"
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
                      aria-label={`Edit ${gatewayLabel(bot)}`}
                      title={`Edit ${gatewayLabel(bot)}`}
                    >
                      <Settings className="h-4 w-4" />
                    </button>
                    <button
                      type="button"
                      onClick={() => handleToggleActive(bot)}
                      className={cn(
                        'p-2 rounded-lg transition-colors',
                        bot.active
                          ? 'text-emerald-400/60 hover:text-emerald-400 hover:bg-emerald-500/10'
                          : 'text-white/40 hover:text-white hover:bg-white/[0.06]'
                      )}
                      aria-label={`${bot.active ? 'Deactivate' : 'Activate'} ${gatewayLabel(bot)}`}
                      title={bot.active ? 'Deactivate' : 'Activate'}
                    >
                      {bot.active ? (
                        <Power className="h-4 w-4" />
                      ) : (
                        <PowerOff className="h-4 w-4" />
                      )}
                    </button>
                    <button
                      type="button"
                      onClick={() => handleDelete(bot)}
                      className="p-2 rounded-lg text-red-400/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                      aria-label={`Delete ${gatewayLabel(bot)}`}
                      title={`Delete ${gatewayLabel(bot)}`}
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  </div>
                </div>

                {/* Expandable details - show chats */}
                <button
                  type="button"
                  onClick={() => toggleExpand(bot.id)}
                  aria-label={`${expandedBots.has(bot.id) ? 'Collapse' : 'Expand'} ${gatewayLabel(bot)} details`}
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
                {expandedBots.has(bot.id) && (() => {
                  const tab = activeTabByBot[bot.id] || 'conversations';
                  const memView = memoryViewByBot[bot.id] || 'entries';
                  const chats = chatsByBot[bot.id] || [];
                  const acts = actionsByBot[bot.id] || [];
                  const sched = scheduledByBot[bot.id] || [];
                  const mem = memoryByBot[bot.id] || [];
                  const autos = automationsByBot[bot.id] || [];
                  const searchHits = memorySearchByBot[bot.id] || [];
                  const convQuery = (conversationQueryByBot[bot.id] || '').toLowerCase();
                  const shownChats = convQuery
                    ? chats.filter((c) =>
                        `${c.chat_title || ''} ${c.chat_id} ${getMissionTitle(c.mission_id)}`
                          .toLowerCase()
                          .includes(convQuery)
                      )
                    : chats;

                  // Derive per-user profiles from user-scoped memory entries.
                  const profileMap = new Map<string, { key: string; name: string; sub: string; entries: AssistantGatewayMemoryEntry[]; latest: string }>();
                  for (const e of mem) {
                    if (e.scope !== 'user') continue;
                    const key = e.subject_user_id != null ? `u${e.subject_user_id}` : (e.subject_username || 'unknown');
                    const name = e.subject_display_name || e.subject_username || (e.subject_user_id != null ? `User ${e.subject_user_id}` : 'Unknown user');
                    const sub = e.subject_username ? `@${e.subject_username}` : (e.subject_user_id != null ? String(e.subject_user_id) : '');
                    const cur = profileMap.get(key);
                    if (cur) {
                      cur.entries.push(e);
                      if (e.updated_at > cur.latest) cur.latest = e.updated_at;
                    } else {
                      profileMap.set(key, { key, name, sub, entries: [e], latest: e.updated_at });
                    }
                  }
                  const profiles = Array.from(profileMap.values()).sort((a, b) => (a.latest < b.latest ? 1 : -1));

                  // Real "last activity" health signal derived from loaded events.
                  const activityStamps = [
                    ...acts.map((a) => a.updated_at),
                    ...sched.map((s) => s.sent_at || s.send_at),
                    ...chats.map((c) => c.created_at),
                  ].filter(Boolean) as string[];
                  const lastActivity = activityStamps.length
                    ? activityStamps.reduce((max, t) => (t > max ? t : max))
                    : null;

                  const tabCount = (id: GatewayTab): number | null => {
                    if (id === 'conversations') return chats.length || null;
                    if (id === 'actions') return acts.length || null;
                    if (id === 'scheduled') return sched.length || null;
                    if (id === 'tasks') return autos.length || null;
                    if (id === 'memory') return mem.length || null;
                    if (id === 'skills') return hermesSkills.length || null;
                    return null;
                  };

                  return (
                    <div className="border-t border-white/[0.04]">
                      {/* Compact meta strip */}
                      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 py-2 text-[10px] text-white/30 border-b border-white/[0.04]">
                        <span className="font-mono text-white/40 truncate max-w-[220px]" title={bot.id}>{bot.id}</span>
                        <span className="text-white/15">·</span>
                        <span>
                          {BACKEND_LABELS[bot.default_backend || 'claudecode'] || bot.default_backend || 'Claude Code'}
                          {bot.default_model_override ? ` / ${bot.default_model_override}` : ''}
                        </span>
                        <span className="text-white/15">·</span>
                        <span>
                          {bot.allowed_chat_ids?.length
                            ? `${bot.allowed_chat_ids.length} allowed chat${bot.allowed_chat_ids.length === 1 ? '' : 's'}`
                            : 'all chats'}
                        </span>
                        <span className="text-white/15">·</span>
                        <span>created {relTime(bot.created_at)}</span>
                        {lastActivity && (
                          <>
                            <span className="text-white/15">·</span>
                            <span className="inline-flex items-center gap-1">
                              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                              active {relTime(lastActivity)}
                            </span>
                          </>
                        )}
                      </div>

                      {/* Tab bar */}
                      <div className="flex items-center gap-0.5 overflow-x-auto px-2 border-b border-white/[0.06]">
                        {GATEWAY_TABS.map((t) => {
                          const active = tab === t.id;
                          const count = tabCount(t.id);
                          return (
                            <button
                              key={t.id}
                              type="button"
                              onClick={() => setActiveTabByBot((prev) => ({ ...prev, [bot.id]: t.id }))}
                              className={cn(
                                'shrink-0 px-2.5 py-2 text-xs font-medium border-b-2 -mb-px transition-colors',
                                active
                                  ? 'border-indigo-400 text-white'
                                  : 'border-transparent text-white/40 hover:text-white/70'
                              )}
                            >
                              {t.label}
                              {count != null && (
                                <span className={cn('ml-1.5 text-[10px]', active ? 'text-white/40' : 'text-white/25')}>{count}</span>
                              )}
                            </button>
                          );
                        })}
                      </div>

                      <div className="px-4 py-2 min-h-[64px]">
                        {/* CONVERSATIONS */}
                        {tab === 'conversations' && (
                          <div>
                            <div className="relative mb-1">
                              <Search className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-white/25" />
                              <input
                                type="text"
                                placeholder="Search conversations..."
                                value={conversationQueryByBot[bot.id] || ''}
                                onChange={(e) => setConversationQueryByBot((prev) => ({ ...prev, [bot.id]: e.target.value }))}
                                className="w-full rounded-lg bg-white/[0.03] border border-white/[0.06] pl-8 pr-3 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                              />
                            </div>
                            {loadingChats.has(bot.id) ? (
                              <div className="flex items-center gap-2 px-1 py-3 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                            ) : shownChats.length === 0 ? (
                              <EmptyHint icon={MessageCircle}>
                                {chats.length === 0
                                  ? 'No conversations yet. Message the gateway to start one.'
                                  : 'No conversations match your search.'}
                              </EmptyHint>
                            ) : (
                              <div>
                                {shownChats.map((chat) => {
                                  const testKey = `${bot.id}:${chat.chat_id}`;
                                  return (
                                    <div key={chat.id} className={cn(ROW, ROW_PAD, 'items-center')}>
                                      <div className="flex-1 min-w-0">
                                        <p className="text-xs text-white/70 truncate">{chat.chat_title || `Chat ${chat.chat_id}`}</p>
                                        <p className="text-[10px] text-white/35 truncate">{getMissionTitle(chat.mission_id)}</p>
                                      </div>
                                      <button
                                        type="button"
                                        onClick={() => void handleSendTest(bot, chat)}
                                        disabled={sendingTest.has(testKey)}
                                        className="shrink-0 inline-flex items-center gap-1 rounded-md border border-white/[0.08] px-2 py-1 text-[10px] text-white/50 opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-white hover:border-white/[0.16] transition disabled:opacity-100 disabled:text-white/30"
                                        title="Send a test message to this chat"
                                      >
                                        {sendingTest.has(testKey) ? <Loader className="h-3 w-3 animate-spin" /> : <Send className="h-3 w-3" />}
                                        Test
                                      </button>
                                      <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{relTime(chat.created_at)}</span>
                                    </div>
                                  );
                                })}
                              </div>
                            )}
                          </div>
                        )}

                        {/* ACTIONS */}
                        {tab === 'actions' && (
                          loadingActions.has(bot.id) ? (
                            <div className="flex items-center gap-2 px-1 py-3 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                          ) : acts.length === 0 ? (
                            <EmptyHint icon={Activity}>No gateway actions recorded yet.</EmptyHint>
                          ) : (
                            <div>
                              {acts.map((action) => {
                                const rk = `act:${action.id}`;
                                const open = expandedRows.has(rk);
                                return (
                                  <div key={action.id} className={ROW}>
                                    <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left hover:bg-white/[0.02] rounded')}>
                                      <span className={cn('mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full', STATUS_DOT[action.status] || 'bg-white/20')} />
                                      <div className="flex-1 min-w-0">
                                        <p className="text-xs text-white/70 truncate">
                                          {action.target_chat_title || `Chat ${action.target_chat_id}`}
                                          <span className="text-white/30"> · {action.action_kind}</span>
                                          {action.delay_seconds > 0 ? <span className="text-white/30"> · +{action.delay_seconds}s</span> : null}
                                        </p>
                                        <p className="text-[11px] text-white/40 truncate">{action.text}</p>
                                      </div>
                                      <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{relTime(action.updated_at)}</span>
                                    </button>
                                    {open && (
                                      <div className="px-4 pb-2 -mt-0.5 space-y-1">
                                        <p className="text-[11px] text-white/55 whitespace-pre-wrap">{action.text}</p>
                                        <p className="text-[10px] text-white/30">
                                          Target: {action.target_kind} {action.target_value}
                                          {action.last_error ? ` · ${action.last_error}` : ''}
                                        </p>
                                      </div>
                                    )}
                                  </div>
                                );
                              })}
                            </div>
                          )
                        )}

                        {/* SCHEDULED */}
                        {tab === 'scheduled' && (
                          loadingScheduled.has(bot.id) ? (
                            <div className="flex items-center gap-2 px-1 py-3 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                          ) : sched.length === 0 ? (
                            <EmptyHint icon={Calendar}>No scheduled messages for this gateway.</EmptyHint>
                          ) : (
                            <div>
                              {sched.map((message) => {
                                const rk = `sch:${message.id}`;
                                const open = expandedRows.has(rk);
                                return (
                                  <div key={message.id} className={ROW}>
                                    <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left hover:bg-white/[0.02] rounded')}>
                                      <span className={cn('mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full', STATUS_DOT[message.status] || 'bg-white/20')} />
                                      <div className="flex-1 min-w-0">
                                        <p className="text-xs text-white/70 truncate">{message.chat_title || `Chat ${message.chat_id}`}</p>
                                        <p className="text-[11px] text-white/40 truncate">{message.text}</p>
                                      </div>
                                      <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{relTime(message.send_at)}</span>
                                    </button>
                                    {open && (
                                      <div className="px-4 pb-2 -mt-0.5 space-y-1">
                                        <p className="text-[11px] text-white/55 whitespace-pre-wrap">{message.text}</p>
                                        <p className="text-[10px] text-white/30">
                                          Send at {new Date(message.send_at).toLocaleString()}
                                          {message.sent_at ? ` · sent ${new Date(message.sent_at).toLocaleString()}` : ''}
                                          {message.last_error ? ` · ${message.last_error}` : ''}
                                        </p>
                                      </div>
                                    )}
                                  </div>
                                );
                              })}
                            </div>
                          )
                        )}

                        {/* TASKS — scheduled automations on this gateway's missions */}
                        {tab === 'tasks' && (
                          loadingAutomations.has(bot.id) ? (
                            <div className="flex items-center gap-2 px-1 py-3 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                          ) : autos.length === 0 ? (
                            <EmptyHint icon={ListChecks}>
                              No scheduled tasks for this gateway. Automations created on its missions (daily digests, reminders, audits) appear here.
                            </EmptyHint>
                          ) : (
                            <div>
                              {autos.map((a) => {
                                const rk = `auto:${a.id}`;
                                const open = expandedRows.has(rk);
                                return (
                                  <div key={a.id} className={ROW}>
                                    <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left hover:bg-white/[0.02] rounded')}>
                                      <span className={cn('mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full', a.active ? 'bg-emerald-400' : 'bg-white/20')} />
                                      <div className="flex-1 min-w-0">
                                        <p className="text-xs text-white/70 truncate">
                                          {automationLabel(a.command_source)}
                                          <span className="text-white/30"> · {triggerLabel(a.trigger)}</span>
                                          {!a.active ? <span className="text-white/30"> · paused</span> : null}
                                        </p>
                                        <p className="text-[10px] text-white/35 truncate">{getMissionTitle(a.mission_id)}</p>
                                      </div>
                                      <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">
                                        {a.last_triggered_at ? relTime(a.last_triggered_at) : 'never run'}
                                      </span>
                                    </button>
                                    {open && (
                                      <div className="px-4 pb-2 -mt-0.5 space-y-1 text-[10px] text-white/30">
                                        <p>Trigger: {triggerLabel(a.trigger)} · {a.active ? 'active' : 'paused'}</p>
                                        <p>Mission: {getMissionTitle(a.mission_id)}</p>
                                        <p>Created {relTime(a.created_at)}{a.last_triggered_at ? ` · last run ${relTime(a.last_triggered_at)}` : ''}</p>
                                      </div>
                                    )}
                                  </div>
                                );
                              })}
                            </div>
                          )
                        )}

                        {/* MEMORY */}
                        {tab === 'memory' && (
                          <div>
                            <div className="flex items-center gap-1 mb-2">
                              {(['entries', 'profiles'] as const).map((v) => (
                                <button
                                  key={v}
                                  type="button"
                                  onClick={() => setMemoryViewByBot((prev) => ({ ...prev, [bot.id]: v }))}
                                  className={cn(
                                    'rounded-md px-2 py-1 text-[11px] font-medium transition-colors',
                                    memView === v ? 'bg-white/[0.06] text-white/80' : 'text-white/40 hover:text-white/70'
                                  )}
                                >
                                  {v === 'entries' ? 'Entries' : 'Profiles'}
                                </button>
                              ))}
                            </div>

                            {memView === 'entries' ? (
                              <div>
                                <div className="relative mb-1">
                                  <Search className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-white/25" />
                                  <input
                                    type="text"
                                    placeholder="Search structured memory..."
                                    value={memorySearchQueryByBot[bot.id] || ''}
                                    onChange={(e) => setMemorySearchQueryByBot((prev) => ({ ...prev, [bot.id]: e.target.value }))}
                                    onKeyDown={(e) => { if (e.key === 'Enter') void loadMemorySearch(bot.id); }}
                                    className="w-full rounded-lg bg-white/[0.03] border border-white/[0.06] pl-8 pr-3 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                                  />
                                </div>
                                {loadingMemorySearch.has(bot.id) ? (
                                  <div className="flex items-center gap-2 px-1 py-2 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Searching...</div>
                                ) : (bot.id in memorySearchByBot) && searchHits.length > 0 ? (
                                  <div className="mb-2">
                                    <p className="px-1 py-1 text-[10px] uppercase tracking-wide text-white/25">Ranked matches</p>
                                    {searchHits.map((hit) => (
                                      <div key={`${hit.entry.id}-s`} className={cn(ROW, ROW_PAD)}>
                                        <div className="flex-1 min-w-0">
                                          <p className="text-xs text-white/70 truncate">{hit.entry.label ? `${hit.entry.label} · ` : ''}{hit.entry.value}</p>
                                          <p className="text-[10px] text-white/30 truncate">{hit.reasons.join(' · ')}</p>
                                        </div>
                                        <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{hit.score.toFixed(1)}</span>
                                      </div>
                                    ))}
                                  </div>
                                ) : (bot.id in memorySearchByBot) && memorySearchQueryByBot[bot.id]?.trim() ? (
                                  <p className="px-1 py-1 text-[11px] text-white/30 italic">No ranked matches for this query.</p>
                                ) : null}

                                {loadingMemory.has(bot.id) ? (
                                  <div className="flex items-center gap-2 px-1 py-2 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                                ) : mem.length === 0 ? (
                                  <EmptyHint icon={Brain}>No structured memory captured yet.</EmptyHint>
                                ) : (
                                  <div>
                                    {mem.map((entry) => {
                                      const rk = `mem:${entry.id}`;
                                      const open = expandedRows.has(rk);
                                      return (
                                        <div key={entry.id} className={ROW}>
                                          <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left hover:bg-white/[0.02] rounded')}>
                                            <div className="flex-1 min-w-0">
                                              <p className="text-xs text-white/70 truncate">
                                                <span className="text-white/35">{entry.kind} · {entry.scope}</span>
                                                {' · '}
                                                {entry.scope === 'user'
                                                  ? (entry.subject_display_name || entry.subject_username || `User ${entry.subject_user_id}`)
                                                  : `Chat ${entry.chat_id}`}
                                                {entry.label ? ` · ${entry.label}` : ''}
                                              </p>
                                              <p className="text-[11px] text-white/40 truncate">{entry.value}</p>
                                            </div>
                                            <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{relTime(entry.updated_at)}</span>
                                          </button>
                                          {open && (
                                            <div className="px-4 pb-2 -mt-0.5">
                                              <p className="text-[11px] text-white/55 whitespace-pre-wrap">{entry.value}</p>
                                            </div>
                                          )}
                                        </div>
                                      );
                                    })}
                                  </div>
                                )}
                              </div>
                            ) : (
                              loadingMemory.has(bot.id) ? (
                                <div className="flex items-center gap-2 px-1 py-2 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                              ) : profiles.length === 0 ? (
                                <EmptyHint icon={User}>No user profiles yet. User-scoped memory builds these automatically.</EmptyHint>
                              ) : (
                                <div>
                                  {profiles.map((p) => {
                                    const rk = `prof:${p.key}`;
                                    const open = expandedRows.has(rk);
                                    return (
                                      <div key={p.key} className={ROW}>
                                        <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left items-center hover:bg-white/[0.02] rounded')}>
                                          <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-white/[0.06] text-[10px] font-medium text-white/60">{(p.name.replace(/^@/, '') || p.name).charAt(0).toUpperCase()}</span>
                                          <div className="flex-1 min-w-0">
                                            <p className="text-xs text-white/70 truncate">{p.name}{p.sub && p.sub.replace(/^@/, '').toLowerCase() !== p.name.replace(/^@/, '').toLowerCase() ? <span className="text-white/30"> · {p.sub}</span> : null}</p>
                                            <p className="text-[10px] text-white/35">{p.entries.length} fact{p.entries.length === 1 ? '' : 's'}</p>
                                          </div>
                                          <span className="shrink-0 font-mono text-[10px] text-white/25 tabular-nums">{relTime(p.latest)}</span>
                                        </button>
                                        {open && (
                                          <div className="pl-9 pr-2 pb-2 space-y-1">
                                            {p.entries.map((e) => (
                                              <div key={e.id} className="flex items-start gap-2">
                                                <span className="mt-1 h-1 w-1 shrink-0 rounded-full bg-white/25" />
                                                <p className="text-[11px] text-white/55"><span className="text-white/35">{e.label ? `${e.label}: ` : ''}</span>{e.value}</p>
                                              </div>
                                            ))}
                                          </div>
                                        )}
                                      </div>
                                    );
                                  })}
                                </div>
                              )
                            )}
                          </div>
                        )}

                        {/* SKILLS — skills the Hermes runtime installed for itself */}
                        {tab === 'skills' && (
                          skillsLoading ? (
                            <div className="flex items-center gap-2 px-1 py-3 text-xs text-white/40"><Loader className="h-3 w-3 animate-spin" /> Loading...</div>
                          ) : hermesSkillsData && !hermesSkillsData.available ? (
                            <EmptyHint icon={Sparkles}>
                              Hermes skills directory not found. These are managed by the Hermes runtime and appear once it is installed.
                            </EmptyHint>
                          ) : hermesSkills.length === 0 ? (
                            <EmptyHint icon={Sparkles}>
                              The Hermes runtime has no skills installed yet.
                            </EmptyHint>
                          ) : (
                            <div>
                              {hermesSkills.map((skill: HermesSkill) => {
                                const rk = `skill:${skill.path}`;
                                const open = expandedRows.has(rk);
                                return (
                                  <div key={skill.path} className={ROW}>
                                    <button type="button" onClick={() => toggleRow(rk)} className={cn(ROW_PAD, 'w-full text-left hover:bg-white/[0.02] rounded')}>
                                      <Sparkles className="mt-0.5 h-3.5 w-3.5 shrink-0 text-white/30" />
                                      <div className="flex-1 min-w-0">
                                        <p className="text-xs text-white/70 truncate">
                                          {skill.name}
                                          {skill.category && <span className="text-white/30"> · {skill.category}</span>}
                                          {skill.version && <span className="text-white/25"> · v{skill.version}</span>}
                                        </p>
                                        {skill.description && (
                                          <p className={cn('text-[11px] text-white/40', open ? 'whitespace-pre-wrap' : 'truncate')}>{skill.description}</p>
                                        )}
                                      </div>
                                    </button>
                                  </div>
                                );
                              })}
                            </div>
                          )
                        )}
                      </div>
                    </div>
                  );
                })()}
              </div>
            ))}
          </div>
        )}
        </section>
      </div>

      {/* Create Dialog */}
      {showCreateDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="assistant-create-gateway-title"
            className="w-full max-w-lg max-h-[90vh] overflow-y-auto p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]"
          >
            <h3 id="assistant-create-gateway-title" className="text-lg font-medium text-white mb-4">
              Add Assistant Gateway
            </h3>
            <div
              className={cn(
                'mb-4 flex gap-3 rounded-lg border p-3',
                hermesRuntimeReady
                  ? 'border-amber-500/20 bg-amber-500/[0.06]'
                  : 'border-sky-500/20 bg-sky-500/[0.05]'
              )}
            >
              <AlertTriangle
                className={cn(
                  'mt-0.5 h-4 w-4 shrink-0',
                  hermesRuntimeReady ? 'text-amber-300' : 'text-sky-300'
                )}
              />
              <p className="text-xs leading-5 text-white/55">
                {hermesRuntimeReady
                  ? 'Hermes runtime is active. Do not add a compatibility gateway for a bot token Hermes already owns.'
                  : 'This compatibility gateway registers the Telegram webhook until Hermes owns the bot.'}
              </p>
            </div>
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
                    : ['claudecode', 'opencode', 'codex', 'gemini', 'grok', 'chatgpt_ui'].map((id) => (
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
                  <option value="">
                    {createBackend === 'claudecode'
                      ? 'No override (configured default; fallback claude-opus-5)'
                      : 'No override (use default)'}
                  </option>
                  {(() => {
                    const groupedOptions = new Map<string, Array<{ value: string; label: string; description?: string }>>();
                    for (const option of modelOptions) {
                      const providerName = option.label.split(/\s[—·]\s/)[0] || 'Other';
                      if (!groupedOptions.has(providerName)) groupedOptions.set(providerName, []);
                      groupedOptions.get(providerName)!.push(option);
                    }
                    return Array.from(groupedOptions.entries()).map(([providerName, options]) => (
                      <optgroup key={providerName} label={providerName}>
                        {options.map((option) => {
                          const modelName = option.label.split(/\s[—·]\s/)[1] || option.label;
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
                <p className="text-xs text-white/40 mb-3">Gateway behavior</p>
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
                {creating ? 'Creating...' : 'Add Gateway'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Edit Dialog */}
      {editingBot && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="assistant-edit-gateway-title"
            className="w-full max-w-lg max-h-[90vh] overflow-y-auto p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]"
          >
            <h3 id="assistant-edit-gateway-title" className="text-lg font-medium text-white mb-4">
              Edit @{editingBot.bot_username || 'bot'}
            </h3>
            {hermesRuntimeReady && (
              <div className="mb-4 flex gap-3 rounded-lg border border-amber-500/20 bg-amber-500/[0.06] p-3">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-300" />
                <p className="text-xs leading-5 text-white/55">
                  Hermes runtime is active. Keep this compatibility gateway inactive for bot tokens already moved to Hermes.
                </p>
              </div>
            )}
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
                    : ['claudecode', 'opencode', 'codex', 'gemini', 'grok', 'chatgpt_ui'].map((id) => (
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
                  <option value="">
                    {(editBackend || 'claudecode') === 'claudecode'
                      ? 'No override (configured default; fallback claude-opus-5)'
                      : 'No override (use default)'}
                  </option>
                  {(() => {
                    const groupedOptions = new Map<string, Array<{ value: string; label: string; description?: string }>>();
                    for (const option of editModelOptions) {
                      const providerName = option.label.split(/\s[—·]\s/)[0] || 'Other';
                      if (!groupedOptions.has(providerName)) groupedOptions.set(providerName, []);
                      groupedOptions.get(providerName)!.push(option);
                    }
                    return Array.from(groupedOptions.entries()).map(([providerName, options]) => (
                      <optgroup key={providerName} label={providerName}>
                        {options.map((option) => {
                          const modelName = option.label.split(/\s[—·]\s/)[1] || option.label;
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

              {/* Gateway behavior */}
              <div className="border-t border-white/[0.06] pt-4">
                <p className="text-xs text-white/40 mb-3">Gateway behavior</p>
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
