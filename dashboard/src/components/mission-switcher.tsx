'use client';

import { useEffect, useRef, useState, useMemo, useCallback } from 'react';
import {
  Search,
  XCircle,
  Check,
  Loader2,
  RotateCcw,
  AlertTriangle,
  MessageSquarePlus,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { searchMissions, type Mission, type RunningMissionInfo } from '@/lib/api';
import { getMissionShortName } from '@/lib/mission-display';
import { STATUS_LABELS, getMissionDotColor, getMissionTitle } from '@/lib/mission-status';

interface MissionSwitcherProps {
  open: boolean;
  onClose: () => void;
  missions: Mission[];
  runningMissions: RunningMissionInfo[];
  currentMissionId?: string | null;
  viewingMissionId?: string | null;
  workspaceNameById?: Record<string, string>;
  onSelectMission: (missionId: string) => Promise<void> | void;
  onCancelMission: (missionId: string) => void;
  onResumeMission?: (missionId: string) => Promise<void> | void;
  onOpenFailingToolCall?: (missionId: string) => Promise<void> | void;
  onFollowUpMission?: (missionId: string) => Promise<void> | void;
  onRefresh?: () => void;
}

function getWorkspaceLabel(
  mission: Mission,
  workspaceNameById?: Record<string, string>
): string | null {
  if (mission.workspace_name) {
    return mission.workspace_name;
  }
  if (mission.workspace_id && workspaceNameById?.[mission.workspace_id]) {
    return workspaceNameById[mission.workspace_id];
  }
  return null;
}

function getMissionDisplayName(
  mission: Mission,
  _workspaceNameById?: Record<string, string>
): string {
  // Use title as primary name when available, fall back to animal codename
  const title = mission.title?.trim();
  if (title) {
    return title.length > 70 ? title.slice(0, 70) + '...' : title;
  }
  return getMissionShortName(mission.id);
}

export function getMissionCardTitle(mission: Mission): string | null {
  // When a backend title exists, it's already shown as the display name,
  // so only surface short_description when it adds distinct context.
  if (mission.title?.trim()) {
    const shortDescription = mission.short_description?.trim();
    if (!shortDescription) return null;
    return hasMeaningfulExtraTokens(mission.title, shortDescription)
      ? shortDescription
      : null;
  }
  // No backend title: display name is the animal codename, so show the
  // first user message as subtitle.
  const title = getMissionTitle(mission, { maxLength: 80, fallback: '' }).trim();
  if (!title) return null;
  return title;
}

export function normalizeMetadataText(text: string): string {
  const lower = text.toLowerCase();
  let sanitized = lower;
  try {
    sanitized = lower.replace(new RegExp('[^\\p{L}\\p{N}\\s]', 'gu'), ' ');
  } catch {
    sanitized = lower.replace(/[!"#$%&'()*+,./:;<=>?@[\\\]^_`{|}~-]/g, ' ');
  }
  return sanitized.replace(/\s+/g, ' ').trim();
}

export function hasMeaningfulExtraTokens(baseText: string, candidateText: string): boolean {
  const base = normalizeMetadataText(baseText);
  const candidate = normalizeMetadataText(candidateText);
  if (!candidate) return false;
  if (!base) return true;

  const baseTokens = new Set(base.split(' ').filter(Boolean));
  const candidateTokens = candidate.split(' ').filter(Boolean);
  return candidateTokens.some((token) => !baseTokens.has(token));
}

export function getMissionCardDescription(
  mission: Mission,
  cardTitle?: string | null
): string | null {
  // When the backend title is used as display name, short_description is
  // already surfaced as cardTitle — don't repeat it.
  if (mission.title?.trim()) return null;

  const shortDescription = mission.short_description?.trim();
  if (!shortDescription) return null;

  const normalizedTitle = (cardTitle ?? '').trim();
  if (normalizedTitle && !hasMeaningfulExtraTokens(normalizedTitle, shortDescription)) {
    return null;
  }
  return shortDescription;
}

function getMissionBackendLabel(mission: Mission): string {
  const backend = mission.backend?.trim();
  if (!backend) return 'claudecode';
  return backend;
}

function getMissionStatusLabel(mission: Mission): string {
  return STATUS_LABELS[mission.status] ?? mission.status ?? 'Unknown';
}

export interface MissionQuickAction {
  action: 'resume' | 'open_failure' | 'follow_up';
  label: string;
  title: string;
}

export function getRunningMissionQuickActions(): MissionQuickAction[] {
  return [
    {
      action: 'follow_up',
      label: 'Follow-up',
      title: 'Start a follow-up mission from this context',
    },
  ];
}

export function getMissionQuickActions(mission: Mission, isRunning: boolean): MissionQuickAction[] {
  if (isRunning) {
    return getRunningMissionQuickActions();
  }

  const actions: MissionQuickAction[] = [];
  if (mission.status === 'failed') {
    actions.push({
      action: 'open_failure',
      label: 'Open Failure',
      title: 'Jump to likely failing tool call',
    });
  }

  if (mission.resumable) {
    switch (mission.status) {
      case 'blocked':
        actions.push({
          action: 'resume',
          label: 'Continue',
          title: 'Continue mission',
        });
        break;
      case 'failed':
        actions.push({
          action: 'resume',
          label: 'Retry',
          title: 'Retry mission',
        });
        break;
      case 'interrupted':
        actions.push({
          action: 'resume',
          label: 'Resume',
          title: 'Resume mission',
        });
        break;
    }
  }

  if (mission.status !== 'active') {
    actions.push({
      action: 'follow_up',
      label: 'Follow-up',
      title: 'Create a follow-up mission',
    });
  }

  return actions;
}

export function getMissionSearchText(mission: Mission): string {
  const title = getMissionCardTitle(mission) ?? '';
  const shortDescription = mission.short_description?.trim() ?? '';
  const titleBase = mission.title?.trim() ?? title;
  const backend = getMissionBackendLabel(mission);
  const status = mission.status ?? '';
  const textParts: string[] = [];

  // Always include animal codename so it's searchable
  textParts.push(getMissionShortName(mission.id));
  if (mission.title?.trim()) {
    textParts.push(mission.title.trim());
  }
  if (title && title !== mission.title?.trim()) {
    textParts.push(title);
  }
  if (
    shortDescription &&
    (textParts.length === 0 || hasMeaningfulExtraTokens(titleBase, shortDescription))
  ) {
    textParts.push(shortDescription);
  }
  if (backend) {
    textParts.push(backend);
  }
  if (status) {
    textParts.push(status);
  }
  if (mission.mission_mode === 'assistant') {
    textParts.push('assistant telegram');
  }
  return textParts.join(' ');
}

export function getRunningMissionSearchText(runningInfo: RunningMissionInfo): string {
  return [runningInfo.mission_id, getMissionShortName(runningInfo.mission_id), runningInfo.state]
    .filter(Boolean)
    .join(' ');
}

export function runningMissionMatchesSearchQuery(
  runningInfo: RunningMissionInfo,
  searchQuery: string
): boolean {
  if (!normalizeMetadataText(searchQuery)) return true;
  return runningMissionSearchRelevanceScore(runningInfo, searchQuery) > 0;
}

const SEARCH_SYNONYMS: Record<string, string[]> = {
  api: ['endpoint', 'http', 'rest', 'rpc'],
  auth: ['login', 'signin', 'oauth', 'credential', 'credentials'],
  blocked: ['stalled', 'waiting'],
  bug: ['issue', 'error', 'fix', 'problem'],
  cd: ['deploy', 'release', 'rollout', 'ship'],
  ci: ['pipeline', 'build', 'integration', 'tests'],
  crash: ['panic', 'exception', 'failure'],
  db: ['database', 'sql', 'sqlite', 'postgres'],
  deploy: ['release', 'rollout', 'ship'],
  error: ['bug', 'issue', 'failure'],
  failed: ['error', 'failure'],
  fix: ['bug', 'issue', 'error', 'repair'],
  issue: ['bug', 'error', 'problem', 'fix'],
  login: ['auth', 'signin', 'oauth', 'credentials'],
  performance: ['perf', 'slow', 'latency', 'optimize'],
  perf: ['performance', 'slow', 'latency', 'optimize'],
  release: ['deploy', 'rollout', 'ship'],
  sid: ['session', 'id', 'sessionid', 'cookie', 'token'],
  signin: ['login', 'auth', 'oauth', 'credentials'],
  slow: ['performance', 'latency', 'timeout', 'stall'],
  sso: ['signin', 'login', 'auth', 'oauth'],
  stalled: ['blocked', 'waiting', 'timeout'],
  timeout: ['slow', 'latency', 'stalled', 'hang'],
  ui: ['ux', 'interface', 'frontend'],
  ux: ['ui', 'interface', 'frontend'],
};

const SEARCH_PHRASE_EXPANSIONS: Record<string, string[]> = {
  cd: ['continuous deployment'],
  ci: ['continuous integration'],
  sid: ['session id'],
  sso: ['single sign on'],
};

const SEARCH_STOPWORDS = new Set([
  'a',
  'an',
  'and',
  'at',
  'did',
  'do',
  'does',
  'for',
  'from',
  'how',
  'i',
  'in',
  'is',
  'it',
  'me',
  'my',
  'of',
  'on',
  'or',
  'our',
  'please',
  'show',
  'that',
  'the',
  'this',
  'to',
  'us',
  'was',
  'we',
  'what',
  'when',
  'where',
  'which',
  'who',
  'why',
  'with',
  'you',
  'your',
]);

interface SearchQueryTerms {
  normalizedQuery: string;
  normalizedCoreQuery: string;
  queryGroups: string[][];
  phraseQueries: string[];
}

function buildSearchQueryTerms(searchQuery: string): SearchQueryTerms | null {
  const normalizedQuery = normalizeMetadataText(searchQuery);
  if (!normalizedQuery) return null;

  const queryTokens = normalizedQuery.split(' ').filter(Boolean);
  if (queryTokens.length === 0) return null;

  const filteredTokens = queryTokens.filter((token) => !SEARCH_STOPWORDS.has(token));
  const effectiveTokens = filteredTokens.length > 0 ? filteredTokens : queryTokens;
  const normalizedCoreQuery = effectiveTokens.join(' ');

  const queryGroups = effectiveTokens
    .map(expandQueryTokenGroup)
    .filter((group) => group.length > 0);
  if (queryGroups.length === 0) return null;

  const phraseQueries = Array.from(
    new Set([
      normalizedCoreQuery,
      ...effectiveTokens.flatMap((token) =>
        (SEARCH_PHRASE_EXPANSIONS[token] ?? [])
          .map((phrase) => normalizeMetadataText(phrase))
          .filter(Boolean)
      ),
    ].filter(Boolean))
  );

  return {
    normalizedQuery,
    normalizedCoreQuery,
    queryGroups,
    phraseQueries,
  };
}

function expandQueryTokenGroup(token: string): string[] {
  const normalized = normalizeMetadataText(token);
  if (!normalized) return [];

  const values = new Set<string>();
  values.add(normalized);

  const direct = SEARCH_SYNONYMS[normalized] ?? [];
  for (const candidate of direct) {
    const normalizedCandidate = normalizeMetadataText(candidate);
    if (normalizedCandidate) {
      values.add(normalizedCandidate);
    }
  }

  return Array.from(values);
}

function tokenMatchStrength(token: string, candidate: string): number {
  if (token === candidate) return 1;
  const asciiCandidate = /^[a-z0-9]+$/.test(candidate);
  if (token.startsWith(candidate) && (!asciiCandidate || candidate.length >= 3)) return 0.7;
  if (
    asciiCandidate &&
    token.length >= 5 &&
    candidate.startsWith(token) &&
    candidate.length - token.length <= 2
  ) {
    return 0.65;
  }
  if (candidate.length >= 4 && token.includes(candidate)) return 0.45;
  return 0;
}

function groupMatchStrengthForTokenSet(group: string[], tokenSet: Set<string>): number {
  let best = 0;
  for (const candidate of group) {
    if (!candidate) continue;
    for (const token of tokenSet) {
      const strength = tokenMatchStrength(token, candidate);
      if (strength > best) {
        best = strength;
      }
      if (best >= 1) {
        return best;
      }
    }
  }
  return best;
}

function tokenSetFromText(text: string): Set<string> {
  const normalized = normalizeMetadataText(text);
  return new Set(normalized.split(' ').filter(Boolean));
}

function hashSearchQuery(normalizedQuery: string): string {
  let hash = 2166136261;
  for (let i = 0; i < normalizedQuery.length; i += 1) {
    hash ^= normalizedQuery.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

function getMissionSearchCacheKey(
  mission: Mission,
  normalizedQuery: string,
  workspaceNameById?: Record<string, string>
): string {
  const workspaceLabel = getWorkspaceLabel(mission, workspaceNameById) ?? '';
  return [
    mission.id,
    mission.updated_at ?? '',
    mission.metadata_updated_at ?? '',
    workspaceLabel,
    hashSearchQuery(normalizedQuery),
  ].join('|');
}

function mapServerMissionSearchScores(results: Array<{ mission: Mission; relevance_score: number }>): Map<string, number> {
  const scoreByMissionId = new Map<string, number>();
  for (const result of results) {
    const missionId = result.mission?.id;
    if (!missionId) continue;
    scoreByMissionId.set(missionId, result.relevance_score ?? 0);
  }
  return scoreByMissionId;
}

export function getMissionSearchScore(
  mission: Mission,
  normalizedQuery: string,
  cache: Map<string, number>,
  workspaceNameById?: Record<string, string>,
  serverScoreByMissionId?: Map<string, number> | null
): number {
  const serverScore = serverScoreByMissionId?.get(mission.id);
  if (typeof serverScore === 'number') {
    return serverScore;
  }

  const cacheKey = getMissionSearchCacheKey(mission, normalizedQuery, workspaceNameById);
  let localScore = cache.get(cacheKey);
  if (localScore === undefined) {
    localScore = missionSearchRelevanceScore(mission, normalizedQuery, workspaceNameById);
    cache.set(cacheKey, localScore);
    if (cache.size > 1000) {
      const oldestKey = cache.keys().next().value;
      if (oldestKey) {
        cache.delete(oldestKey);
      }
    }
  }
  return localScore;
}

export function missionSearchRelevanceScore(
  mission: Mission,
  searchQuery: string,
  workspaceNameById?: Record<string, string>
): number {
  const queryTerms = buildSearchQueryTerms(searchQuery);
  if (!queryTerms) return 0;
  const phraseQueries =
    queryTerms.phraseQueries.length > 0
      ? queryTerms.phraseQueries
      : [queryTerms.normalizedCoreQuery || queryTerms.normalizedQuery];

  const displayName = getMissionDisplayName(mission, workspaceNameById);
  const title = getMissionCardTitle(mission) ?? '';
  const shortDescription = mission.short_description?.trim() ?? '';
  const backend = mission.backend?.trim() ?? '';
  const status = mission.status ?? '';
  const combined = `${displayName} ${getMissionSearchText(mission)}`;

  const normalizedCombined = normalizeMetadataText(combined);
  if (!normalizedCombined) return 0;

  const fields = [
    { weight: 5, tokens: tokenSetFromText(displayName) },
    { weight: 8, tokens: tokenSetFromText(title) },
    { weight: 7, tokens: tokenSetFromText(shortDescription) },
    { weight: 3, tokens: tokenSetFromText(backend) },
    { weight: 2, tokens: tokenSetFromText(status) },
    { weight: 1, tokens: tokenSetFromText(combined) },
  ];

  let score = 0;
  for (const group of queryTerms.queryGroups) {
    let bestGroupScore = 0;
    for (const field of fields) {
      const strength = groupMatchStrengthForTokenSet(group, field.tokens);
      if (strength > 0) {
        bestGroupScore = Math.max(bestGroupScore, strength * field.weight);
      }
    }
    if (bestGroupScore <= 0) {
      return 0;
    }
    score += bestGroupScore;
  }

  const phraseBoostTargets = [
    { text: normalizeMetadataText(title), boost: 14 },
    { text: normalizeMetadataText(shortDescription), boost: 12 },
    { text: normalizeMetadataText(displayName), boost: 8 },
    { text: normalizeMetadataText(combined), boost: 5 },
  ];
  for (const target of phraseBoostTargets) {
    if (target.text && phraseQueries.some((phraseQuery) => target.text.includes(phraseQuery))) {
      score += target.boost;
    }
  }

  return score;
}

function runningMissionSearchRelevanceScore(
  runningInfo: RunningMissionInfo,
  searchQuery: string
): number {
  const queryTerms = buildSearchQueryTerms(searchQuery);
  if (!queryTerms) return 0;
  const phraseQueries =
    queryTerms.phraseQueries.length > 0
      ? queryTerms.phraseQueries
      : [queryTerms.normalizedCoreQuery || queryTerms.normalizedQuery];

  const missionId = runningInfo.mission_id ?? '';
  const state = runningInfo.state ?? '';
  const shortName = getMissionShortName(missionId);
  const combined = [missionId, shortName, state].filter(Boolean).join(' ');
  const normalizedCombined = normalizeMetadataText(combined);
  if (!normalizedCombined) return 0;

  const fields = [
    { weight: 6, tokens: tokenSetFromText(state) },
    { weight: 5, tokens: tokenSetFromText(missionId) },
    { weight: 4, tokens: tokenSetFromText(shortName) },
    { weight: 2, tokens: tokenSetFromText(combined) },
  ];

  let score = 0;
  for (const group of queryTerms.queryGroups) {
    let bestGroupScore = 0;
    for (const field of fields) {
      const strength = groupMatchStrengthForTokenSet(group, field.tokens);
      if (strength > 0) {
        bestGroupScore = Math.max(bestGroupScore, strength * field.weight);
      }
    }
    if (bestGroupScore <= 0) return 0;
    score += bestGroupScore;
  }

  const normalizedState = normalizeMetadataText(state);
  if (normalizedState && phraseQueries.some((phraseQuery) => normalizedState.includes(phraseQuery))) {
    score += 4;
  }
  if (phraseQueries.some((phraseQuery) => normalizedCombined.includes(phraseQuery))) {
    score += 6;
  }

  return score;
}

export function missionMatchesSearchQuery(
  mission: Mission,
  searchQuery: string,
  workspaceNameById?: Record<string, string>
): boolean {
  if (!normalizeMetadataText(searchQuery)) return true;
  return missionSearchRelevanceScore(mission, searchQuery, workspaceNameById) > 0;
}

export function MissionSwitcher({
  open,
  onClose,
  missions,
  runningMissions,
  currentMissionId,
  viewingMissionId,
  workspaceNameById,
  onSelectMission,
  onCancelMission,
  onResumeMission,
  onOpenFailingToolCall,
  onFollowUpMission,
  onRefresh,
}: MissionSwitcherProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const focusTimeoutRef = useRef<number | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loadingMissionId, setLoadingMissionId] = useState<string | null>(null);
  const [serverScoreByMissionId, setServerScoreByMissionId] = useState<Map<string, number> | null>(
    null
  );
  const [serverSearchLoading, setServerSearchLoading] = useState(false);
  const searchScoreCacheRef = useRef<Map<string, number>>(new Map());
  const latestSearchRequestIdRef = useRef(0);
  const normalizedSearchQuery = useMemo(
    () => normalizeMetadataText(searchQuery),
    [searchQuery]
  );

  // Handle mission selection with loading state
  const handleSelect = useCallback(async (missionId: string) => {
    // Don't allow selecting while already loading
    if (loadingMissionId) return;
    
    setLoadingMissionId(missionId);
    try {
      await onSelectMission(missionId);
      onClose();
    } catch (err) {
      console.error('Failed to load mission:', err);
      // Clear loading state on error so user can try again
      setLoadingMissionId(null);
    }
  }, [loadingMissionId, onSelectMission, onClose]);

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

  // Build a map from boss mission id to its worker missions
  const bossMissionWorkerIds = useMemo(() => {
    const map = new Map<string, Set<string>>();
    for (const m of missions) {
      if (m.parent_mission_id) {
        let set = map.get(m.parent_mission_id);
        if (!set) {
          set = new Set();
          map.set(m.parent_mission_id, set);
        }
        set.add(m.id);
      }
    }
    return map;
  }, [missions]);

  // Build flat list of all selectable items, grouping workers under their boss
  const allItems = useMemo(() => {
    const items: Array<{
      type: 'running' | 'current' | 'recent';
      mission?: Mission;
      runningInfo?: RunningMissionInfo;
      id: string;
      isWorkerOf?: string; // parent boss mission id
      isBoss?: boolean;
    }> = [];

    const addedIds = new Set<string>();

    // Helper to add a running mission + its workers
    const addRunningWithWorkers = (rm: RunningMissionInfo) => {
      if (addedIds.has(rm.mission_id)) return;
      addedIds.add(rm.mission_id);
      const mission = missions.find((m) => m.id === rm.mission_id);
      const workerIds = bossMissionWorkerIds.get(rm.mission_id);
      const isBoss = Boolean(workerIds && workerIds.size > 0);
      items.push({
        type: 'running',
        mission,
        runningInfo: rm,
        id: rm.mission_id,
        isBoss,
      });
      // Add workers grouped under this boss
      if (workerIds) {
        for (const workerId of workerIds) {
          if (addedIds.has(workerId)) continue;
          addedIds.add(workerId);
          const workerMission = missions.find((m) => m.id === workerId);
          const workerRunningInfo = runningMissions.find((r) => r.mission_id === workerId);
          items.push({
            type: workerRunningInfo ? 'running' : 'recent',
            mission: workerMission,
            runningInfo: workerRunningInfo,
            id: workerId,
            isWorkerOf: rm.mission_id,
          });
        }
      }
    };

    // Current mission first if not running
    if (currentMissionId) {
      const currentMission = missions.find((m) => m.id === currentMissionId);
      if (currentMission && !runningMissionIds.has(currentMissionId)) {
        addedIds.add(currentMissionId);
        const workerIds = bossMissionWorkerIds.get(currentMissionId);
        items.push({
          type: 'current',
          mission: currentMission,
          id: currentMissionId,
          isBoss: Boolean(workerIds && workerIds.size > 0),
        });
      }
    }

    // Running missions (boss missions first, then standalone)
    const bosses = runningMissions.filter((rm) => bossMissionWorkerIds.has(rm.mission_id));
    const standalone = runningMissions.filter(
      (rm) => !bossMissionWorkerIds.has(rm.mission_id) && !missions.find((m) => m.id === rm.mission_id)?.parent_mission_id
    );
    const workerOnlyRunning = runningMissions.filter((rm) => {
      const m = missions.find((mi) => mi.id === rm.mission_id);
      return m?.parent_mission_id && !bossMissionWorkerIds.has(rm.mission_id);
    });

    // Add bosses first (with their workers grouped)
    for (const rm of bosses) addRunningWithWorkers(rm);
    // Add standalone running missions
    for (const rm of standalone) addRunningWithWorkers(rm);
    // Add orphan worker running missions (boss not running)
    for (const rm of workerOnlyRunning) {
      if (addedIds.has(rm.mission_id)) continue;
      addedIds.add(rm.mission_id);
      const mission = missions.find((m) => m.id === rm.mission_id);
      items.push({
        type: 'running',
        mission,
        runningInfo: rm,
        id: rm.mission_id,
        isWorkerOf: mission?.parent_mission_id ?? undefined,
      });
    }

    // Recent missions
    recentMissions.forEach((m) => {
      if (addedIds.has(m.id)) return;
      items.push({ type: 'recent', mission: m, id: m.id });
    });

    return items;
  }, [missions, currentMissionId, runningMissions, runningMissionIds, recentMissions, bossMissionWorkerIds]);

  useEffect(() => {
    if (!open) return;
    if (!normalizedSearchQuery) {
      setServerScoreByMissionId(null);
      setServerSearchLoading(false);
      return;
    }

    const requestId = latestSearchRequestIdRef.current + 1;
    latestSearchRequestIdRef.current = requestId;
    let cancelled = false;
    const debounce = window.setTimeout(() => {
      if (cancelled || latestSearchRequestIdRef.current !== requestId) return;
      setServerSearchLoading(true);
      void searchMissions(normalizedSearchQuery, { limit: 100 })
        .then((results) => {
          if (cancelled || latestSearchRequestIdRef.current !== requestId) return;
          setServerScoreByMissionId(mapServerMissionSearchScores(results));
        })
        .catch((error) => {
          if (cancelled || latestSearchRequestIdRef.current !== requestId) return;
          console.warn('Mission search endpoint unavailable, falling back to local scoring:', error);
          setServerScoreByMissionId(null);
        })
        .finally(() => {
          if (cancelled || latestSearchRequestIdRef.current !== requestId) return;
          setServerSearchLoading(false);
        });
    }, 120);

    return () => {
      cancelled = true;
      window.clearTimeout(debounce);
    };
  }, [open, normalizedSearchQuery]);

  // Filter items by search query
  const filteredItems = useMemo(() => {
    if (!normalizedSearchQuery) return allItems;

    const cache = searchScoreCacheRef.current;
    const scored = allItems.flatMap((item) => {
      if (!item.mission) {
        if (!item.runningInfo) {
          return [];
        }
        // Always include running missions without a full Mission record so they
        // remain visible during search even when we lack metadata to score.
        const runningScore = runningMissionSearchRelevanceScore(item.runningInfo, normalizedSearchQuery);
        return [{ item, score: Math.max(runningScore, 0.01) }];
      }
      const score = getMissionSearchScore(
        item.mission,
        normalizedSearchQuery,
        cache,
        workspaceNameById,
        serverScoreByMissionId
      );
      if (score <= 0) return [];
      return [{ item, score }];
    });
    scored.sort((a, b) => {
      if (b.score !== a.score) return b.score - a.score;
      const aUpdatedAt = a.item.mission?.updated_at ?? '';
      const bUpdatedAt = b.item.mission?.updated_at ?? '';
      return bUpdatedAt.localeCompare(aUpdatedAt);
    });
    return scored.map(({ item }) => item);
  }, [allItems, normalizedSearchQuery, workspaceNameById, serverScoreByMissionId]);

  useEffect(() => {
    searchScoreCacheRef.current.clear();
  }, [missions, workspaceNameById]);

  // Reset state on open/close
  useEffect(() => {
    if (open) {
      setSearchQuery('');
      setSelectedIndex(0);
      setLoadingMissionId(null);
      // Focus input after animation
      if (focusTimeoutRef.current !== null) {
        window.clearTimeout(focusTimeoutRef.current);
      }
      focusTimeoutRef.current = window.setTimeout(() => {
        inputRef.current?.focus();
        focusTimeoutRef.current = null;
      }, 50);
      // Refresh missions list
      onRefresh?.();
    } else if (focusTimeoutRef.current !== null) {
      window.clearTimeout(focusTimeoutRef.current);
      focusTimeoutRef.current = null;
    }
    return () => {
      if (focusTimeoutRef.current !== null) {
        window.clearTimeout(focusTimeoutRef.current);
        focusTimeoutRef.current = null;
      }
    };
  }, [open, onRefresh]);

  // Reset selected index when filter changes
  useEffect(() => {
    setSelectedIndex(0);
  }, [searchQuery]);

  // Keep selected index in bounds when async rescoring changes list size.
  useEffect(() => {
    setSelectedIndex((prev) => {
      if (filteredItems.length <= 0) return 0;
      return Math.min(prev, filteredItems.length - 1);
    });
  }, [filteredItems.length]);

  // Handle keyboard navigation
  useEffect(() => {
    if (!open) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      // Ignore keyboard nav while loading
      if (loadingMissionId) {
        if (e.key === 'Escape') {
          e.preventDefault();
          // Allow escape to cancel and close
          setLoadingMissionId(null);
          onClose();
        }
        return;
      }
      
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
            handleSelect(filteredItems[selectedIndex].id);
          }
          break;
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose, filteredItems, selectedIndex, handleSelect, loadingMissionId]);

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
          {serverSearchLoading && searchQuery.trim() ? (
            <Loader2 className="h-3.5 w-3.5 text-white/40 animate-spin shrink-0" />
          ) : null}
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
              {hasCurrent && !normalizedSearchQuery && (
                <div className="px-3 pt-1 pb-2">
                  <span className="text-[10px] font-medium uppercase tracking-wider text-white/30">
                    Current
                  </span>
                </div>
              )}
              {filteredItems.map((item, index) => {
                // Show section headers only when not searching
                const isWorkerItem = Boolean(item.isWorkerOf);
                const showRunningHeader =
                  !normalizedSearchQuery &&
                  item.type === 'running' &&
                  !isWorkerItem &&
                  (index === 0 ||
                    (index === 1 && hasCurrent) ||
                    filteredItems[index - 1]?.type !== 'running');
                const showRecentHeader =
                  !normalizedSearchQuery &&
                  item.type === 'recent' &&
                  !isWorkerItem &&
                  filteredItems[index - 1]?.type !== 'recent';

                const mission = item.mission;
                const isSelected = index === selectedIndex;
                const isViewing = item.id === viewingMissionId;
                const isRunning = item.type === 'running' || Boolean(item.runningInfo);
                const runningInfo = item.runningInfo;
                const missionQuickActions = mission
                  ? getMissionQuickActions(mission, isRunning)
                  : isRunning
                    ? getRunningMissionQuickActions()
                    : [];

                const stallInfo =
                  isRunning && runningInfo?.health?.status === 'stalled'
                    ? runningInfo.health
                    : null;
                const isStalled = Boolean(stallInfo);
                const isSeverlyStalled = stallInfo?.severity === 'severe';
                const isLoading = loadingMissionId === item.id;
                const cardTitle = mission ? getMissionCardTitle(mission) : null;
                const cardDescription = mission
                  ? getMissionCardDescription(mission, cardTitle)
                  : null;
                const hasProgress = runningInfo && runningInfo.subtask_total > 0;

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
                    <a
                      href={`/control?mission=${item.id}`}
                      data-selected={isSelected}
                      onClick={(e) => {
                        e.preventDefault();
                        handleSelect(item.id);
                      }}
                      className={cn(
                        'group flex items-center gap-3 py-2 mx-2 rounded-lg cursor-pointer transition-colors no-underline',
                        isWorkerItem ? 'px-3 ml-6 border-l-2 border-white/[0.06]' : 'px-3',
                        isSelected
                          ? 'bg-indigo-500/15 text-white'
                          : 'text-white/70 hover:bg-white/[0.04]',
                        isSeverlyStalled && 'bg-red-500/10',
                        isStalled && !isSeverlyStalled && 'bg-amber-500/10',
                        isLoading && 'bg-indigo-500/20 pointer-events-none',
                        loadingMissionId && !isLoading && 'opacity-50 pointer-events-none'
                      )}
                    >
                      {/* Status dot or loading spinner */}
                      {isLoading ? (
                        <Loader2 className="h-4 w-4 text-indigo-400 animate-spin shrink-0" />
                      ) : (
                        <div
                          className={cn(
                            'h-2 w-2 rounded-full shrink-0',
                            mission
                              ? getMissionDotColor(mission.status, isRunning)
                              : 'bg-gray-400',
                            isRunning &&
                              runningInfo?.state === 'running' &&
                              'animate-pulse'
                          )}
                        />
                      )}

                      {/* Mission info */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className={cn("font-medium truncate", isWorkerItem ? "text-[13px]" : "text-sm")}>
                            {mission
                              ? getMissionDisplayName(mission, workspaceNameById)
                              : getMissionShortName(item.id)}
                          </span>
                          {mission && (() => {
                            const ws = getWorkspaceLabel(mission, workspaceNameById);
                            return ws ? (
                              <span className="inline-flex items-center rounded bg-white/[0.06] px-1.5 py-0.5 text-[10px] text-white/40 shrink-0 max-w-[80px] truncate">
                                {ws}
                              </span>
                            ) : null;
                          })()}
                          {mission?.mission_mode === 'assistant' && (
                            <span className="inline-flex items-center rounded bg-indigo-500/10 border border-indigo-500/20 px-1 py-0.5 text-[8px] font-medium text-indigo-400 shrink-0">
                              Assistant
                            </span>
                          )}
                          {item.isBoss && (
                            <span className="inline-flex items-center rounded bg-violet-500/10 border border-violet-500/20 px-1 py-0.5 text-[8px] font-medium text-violet-400 shrink-0">
                              Boss
                            </span>
                          )}
                          {isWorkerItem && (
                            <span className="inline-flex items-center rounded bg-cyan-500/10 border border-cyan-500/20 px-1 py-0.5 text-[8px] font-medium text-cyan-400 shrink-0">
                              W
                            </span>
                          )}
                          {!item.isBoss && !isWorkerItem && mission?.parent_mission_id && (
                            <span className="inline-flex items-center rounded bg-cyan-500/10 border border-cyan-500/20 px-1 py-0.5 text-[8px] font-medium text-cyan-400 shrink-0">
                              Worker
                            </span>
                          )}
                          {isStalled && (
                            <span className="text-[10px] text-amber-400 tabular-nums shrink-0">
                              {Math.floor(stallInfo?.seconds_since_activity ?? 0)}s
                            </span>
                          )}
                        </div>
                        {mission && (
                          <>
                            {cardTitle && (
                              <p className="text-xs text-white/55 truncate mt-0.5">
                                {cardTitle}
                              </p>
                            )}
                            {cardDescription && (
                              <p className="text-[11px] text-white/40 truncate">
                                {cardDescription}
                              </p>
                            )}
                          </>
                        )}
                        {/* Activity + progress for running missions */}
                        {isRunning && runningInfo && (
                          <div className="flex items-center gap-2 mt-0.5">
                            {runningInfo.current_activity && (
                              <span className="text-[11px] text-white/40 truncate italic">
                                {runningInfo.current_activity}
                              </span>
                            )}
                            {hasProgress && (
                              <span className="text-[10px] font-mono text-white/30 tabular-nums shrink-0">
                                {runningInfo.subtask_completed}/{runningInfo.subtask_total}
                              </span>
                            )}
                          </div>
                        )}
                      </div>

                      {/* Status label or loading text */}
                      <span className="text-[10px] text-white/30 shrink-0">
                        {isLoading
                          ? 'Loading...'
                          : isRunning
                          ? runningInfo?.state || 'running'
                          : mission
                          ? `${getMissionStatusLabel(mission)} · ${getMissionBackendLabel(mission)}`
                          : ''}
                      </span>

                      {/* Viewing indicator */}
                      {isViewing && !isLoading && (
                        <Check className="h-4 w-4 text-indigo-400 shrink-0" />
                      )}

                      {/* Cancel button for running missions */}
                      {isRunning && !isLoading && (
                        <button
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            onCancelMission(item.id);
                          }}
                          className="p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-white/[0.08] text-white/30 hover:text-red-400 transition-all shrink-0"
                          title="Cancel mission"
                        >
                          <XCircle className="h-4 w-4" />
                        </button>
                      )}
                      {missionQuickActions
                        .filter((action) =>
                          action.action === 'resume'
                            ? Boolean(onResumeMission)
                            : action.action === 'open_failure'
                              ? Boolean(onOpenFailingToolCall)
                              : Boolean(onFollowUpMission)
                        )
                        .map((action) => (
                          <button
                            key={`${item.id}-${action.action}`}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              if (action.action === 'resume') {
                                void onResumeMission?.(item.id);
                              } else if (action.action === 'open_failure') {
                                void onOpenFailingToolCall?.(item.id);
                              } else {
                                void onFollowUpMission?.(item.id);
                              }
                              onClose();
                            }}
                            className="px-1.5 py-0.5 rounded opacity-0 group-hover:opacity-100 hover:bg-white/[0.08] text-[10px] text-white/40 hover:text-emerald-300 transition-all shrink-0 inline-flex items-center gap-1"
                            title={action.title}
                          >
                            {action.action === 'resume' ? (
                              <RotateCcw className="h-3 w-3" />
                            ) : action.action === 'open_failure' ? (
                              <AlertTriangle className="h-3 w-3" />
                            ) : (
                              <MessageSquarePlus className="h-3 w-3" />
                            )}
                            {action.label}
                          </button>
                        ))}
                    </a>
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
