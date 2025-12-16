'use client';

import { useEffect, useMemo, useState, useRef } from 'react';
import Link from 'next/link';
import { toast } from 'sonner';
import { cn } from '@/lib/utils';
import { listTasks, listRuns, listMissions, getCurrentMission, streamControl, TaskState, Run, Mission, ControlRunState } from '@/lib/api';
import { formatCents } from '@/lib/utils';
import { ShimmerSidebarItem, ShimmerCard } from '@/components/ui/shimmer';
import { CopyButton } from '@/components/ui/copy-button';
import { useScrollToBottom } from '@/hooks/use-scroll-to-bottom';
import {
  Bot,
  Brain,
  Cpu,
  CheckCircle,
  XCircle,
  Loader,
  Clock,
  Ban,
  ChevronRight,
  ChevronDown,
  Zap,
  GitBranch,
  Target,
  MessageSquare,
  Search,
} from 'lucide-react';

interface AgentNode {
  id: string;
  type: 'Root' | 'Node' | 'ComplexityEstimator' | 'ModelSelector' | 'TaskExecutor' | 'Verifier';
  status: 'running' | 'completed' | 'failed' | 'pending' | 'paused' | 'cancelled';
  name: string;
  description: string;
  budgetAllocated: number;
  budgetSpent: number;
  children?: AgentNode[];
  logs?: string[];
  selectedModel?: string;
  complexity?: number;
}

const agentIcons = {
  Root: Bot,
  Node: GitBranch,
  ComplexityEstimator: Brain,
  ModelSelector: Cpu,
  TaskExecutor: Zap,
  Verifier: Target,
};

const statusConfig = {
  running: { border: 'border-indigo-500/50', bg: 'bg-indigo-500/10', text: 'text-indigo-400' },
  completed: { border: 'border-emerald-500/50', bg: 'bg-emerald-500/10', text: 'text-emerald-400' },
  failed: { border: 'border-red-500/50', bg: 'bg-red-500/10', text: 'text-red-400' },
  pending: { border: 'border-amber-500/50', bg: 'bg-amber-500/10', text: 'text-amber-400' },
  paused: { border: 'border-white/20', bg: 'bg-white/[0.04]', text: 'text-white/40' },
  cancelled: { border: 'border-white/20', bg: 'bg-white/[0.04]', text: 'text-white/40' },
};

function AgentTreeNode({
  agent,
  depth = 0,
  onSelect,
  selectedId,
}: {
  agent: AgentNode;
  depth?: number;
  onSelect: (agent: AgentNode) => void;
  selectedId: string | null;
}) {
  const [expanded, setExpanded] = useState(true);
  const Icon = agentIcons[agent.type];
  const hasChildren = agent.children && agent.children.length > 0;
  const isSelected = selectedId === agent.id;
  const config = statusConfig[agent.status];

  return (
    <div style={{ marginLeft: depth * 24 }}>
      <div
        className={cn(
          'group flex cursor-pointer items-center gap-3 rounded-xl border p-4 transition-all',
          config.border,
          config.bg,
          isSelected && 'ring-1 ring-indigo-500/50'
        )}
        onClick={() => onSelect(agent)}
      >
        {hasChildren && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              setExpanded(!expanded);
            }}
            className="flex h-6 w-6 items-center justify-center rounded-md hover:bg-white/[0.08] transition-colors"
          >
            {expanded ? (
              <ChevronDown className="h-4 w-4 text-white/40" />
            ) : (
              <ChevronRight className="h-4 w-4 text-white/40" />
            )}
          </button>
        )}

        <div className={cn('flex h-10 w-10 items-center justify-center rounded-xl', config.bg)}>
          <Icon className={cn('h-5 w-5', config.text)} />
        </div>

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-white">{agent.name}</span>
            <span className="tag">{agent.type}</span>
          </div>
          <p className="truncate text-xs text-white/40">{agent.description}</p>
        </div>

        <div className="flex items-center gap-4">
          {agent.status === 'running' && (
            <Loader className={cn('h-4 w-4 animate-spin', config.text)} />
          )}
          {agent.status === 'completed' && (
            <CheckCircle className={cn('h-4 w-4', config.text)} />
          )}
          {agent.status === 'failed' && (
            <XCircle className={cn('h-4 w-4', config.text)} />
          )}
          {agent.status === 'pending' && (
            <Clock className={cn('h-4 w-4', config.text)} />
          )}
          {agent.status === 'cancelled' && (
            <Ban className={cn('h-4 w-4', config.text)} />
          )}

          <div className="text-right">
            <div className="text-[10px] uppercase tracking-wider text-white/30">Budget</div>
            <div className="text-sm font-medium text-white tabular-nums">
              {formatCents(agent.budgetSpent)} / {formatCents(agent.budgetAllocated)}
            </div>
          </div>
        </div>
      </div>

      {hasChildren && expanded && (
        <div className="mt-2 space-y-2">
          {agent.children!.map((child) => (
            <AgentTreeNode
              key={child.id}
              agent={child}
              depth={depth + 1}
              onSelect={onSelect}
              selectedId={selectedId}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

export default function AgentsPage() {
  const [missions, setMissions] = useState<Mission[]>([]);
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [controlState, setControlState] = useState<ControlRunState>('idle');
  const [selectedMissionId, setSelectedMissionId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const selectedMission = useMemo(
    () => missions.find((m) => m.id === selectedMissionId) ?? currentMission,
    [missions, selectedMissionId, currentMission]
  );
  const [selectedAgent, setSelectedAgent] = useState<AgentNode | null>(null);
  const [loading, setLoading] = useState(true);
  const fetchedRef = useRef(false);
  const streamCleanupRef = useRef<null | (() => void)>(null);
  
  // Smart scroll for logs
  const { containerRef: logsContainerRef, endRef: logsEndRef } = useScrollToBottom();

  // Stream control events for real-time status
  useEffect(() => {
    streamCleanupRef.current?.();
    
    const cleanup = streamControl((event) => {
      const data: unknown = event.data;
      if (event.type === 'status' && isRecord(data)) {
        const st = data['state'];
        setControlState(typeof st === 'string' ? (st as ControlRunState) : 'idle');
      }
    });
    
    streamCleanupRef.current = cleanup;
    return () => {
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let hasShownError = false;

    const fetchData = async () => {
      try {
        const [missionsData, currentMissionData] = await Promise.all([
          listMissions().catch(() => []),
          getCurrentMission().catch(() => null),
        ]);
        if (cancelled) return;
        
        fetchedRef.current = true;
        setMissions(missionsData);
        setCurrentMission(currentMissionData);
        
        // Auto-select current mission if none selected
        if (!selectedMissionId && currentMissionData) {
          setSelectedMissionId(currentMissionData.id);
        }
        hasShownError = false;
      } catch (error) {
        if (!hasShownError) {
          toast.error('Failed to fetch missions');
          hasShownError = true;
        }
        console.error('Failed to fetch data:', error);
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    fetchData();
    const interval = setInterval(fetchData, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [selectedMissionId]);

  // Filter missions by search query
  const filteredMissions = useMemo(() => {
    if (!searchQuery.trim()) return missions;
    const query = searchQuery.toLowerCase();
    return missions.filter((m) => 
      m.title?.toLowerCase().includes(query) || 
      m.id.toLowerCase().includes(query)
    );
  }, [missions, searchQuery]);

  // Map control state to agent status
  const controlStateToStatus = (state: ControlRunState, missionStatus?: string): AgentNode['status'] => {
    if (state === 'running' || state === 'waiting_for_tool') return 'running';
    if (missionStatus === 'completed') return 'completed';
    if (missionStatus === 'failed') return 'failed';
    return 'pending';
  };

  const mockAgentTree: AgentNode | null = selectedMission
    ? {
        id: 'root',
        type: 'Root',
        status: controlStateToStatus(controlState, selectedMission.status),
        name: 'Root Agent',
        description: selectedMission.title?.slice(0, 50) || 'Mission ' + selectedMission.id.slice(0, 8),
        budgetAllocated: 1000,
        budgetSpent: 50,
        children: [
          {
            id: 'complexity',
            type: 'ComplexityEstimator',
            status: 'completed',
            name: 'Complexity Estimator',
            description: 'Estimate task difficulty',
            budgetAllocated: 10,
            budgetSpent: 5,
            complexity: 0.6,
          },
          {
            id: 'model-selector',
            type: 'ModelSelector',
            status: 'completed',
            name: 'Model Selector',
            description: 'Select optimal model',
            budgetAllocated: 10,
            budgetSpent: 3,
            selectedModel: 'claude-3.5-sonnet',
          },
          {
            id: 'executor',
            type: 'TaskExecutor',
            status: controlStateToStatus(controlState, selectedMission.status),
            name: 'Task Executor',
            description: 'Execute using tools',
            budgetAllocated: 900,
            budgetSpent: 35,
            logs: selectedMission.history.slice(-5).map((h) => h.content.slice(0, 100)),
          },
          {
            id: 'verifier',
            type: 'Verifier',
            status:
              selectedMission.status === 'completed'
                ? 'completed'
                : selectedMission.status === 'failed'
                  ? 'failed'
                  : 'pending',
            name: 'Verifier',
            description: 'Verify task completion',
            budgetAllocated: 80,
            budgetSpent: selectedMission.status === 'completed' ? 7 : 0,
          },
        ],
      }
    : null;

  // Determine if there's active work
  const isActive = controlState !== 'idle';

  return (
    <div className="flex h-screen">
      {/* Mission selector sidebar */}
      <div className="w-64 border-r border-white/[0.06] glass-panel p-4 flex flex-col">
        <h2 className="mb-3 text-sm font-medium text-white">Missions</h2>
        
        {/* Search */}
        <div className="relative mb-4">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-white/30" />
          <input
            type="text"
            placeholder="Search missions..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] py-2 pl-8 pr-3 text-xs text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
          />
        </div>
        
        {/* Current/Active indicator */}
        {isActive && currentMission && (
          <div className="mb-4 p-3 rounded-xl bg-indigo-500/10 border border-indigo-500/30">
            <div className="flex items-center gap-2">
              <Loader className="h-3 w-3 animate-spin text-indigo-400" />
              <span className="text-xs font-medium text-indigo-400">Active</span>
            </div>
            <p className="mt-1 text-xs text-white/60 truncate">
              {currentMission.title || 'Mission ' + currentMission.id.slice(0, 8)}
            </p>
          </div>
        )}
        
        <div className="flex-1 overflow-y-auto space-y-2">
          {loading ? (
            <>
              <ShimmerSidebarItem />
              <ShimmerSidebarItem />
              <ShimmerSidebarItem />
            </>
          ) : filteredMissions.length === 0 && !currentMission ? (
            <p className="text-xs text-white/40 py-2">
              {searchQuery ? 'No missions found' : 'No missions yet'}
            </p>
          ) : (
            <>
              {/* Show current mission first if it exists and matches search */}
              {currentMission && (!searchQuery || currentMission.title?.toLowerCase().includes(searchQuery.toLowerCase())) && (
                <button
                  key={currentMission.id}
                  onClick={() => setSelectedMissionId(currentMission.id)}
                  className={cn(
                    'w-full rounded-xl p-3 text-left transition-all',
                    selectedMissionId === currentMission.id
                      ? 'bg-white/[0.08] border border-indigo-500/50'
                      : 'bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04] hover:border-white/[0.08]'
                  )}
                >
                  <div className="flex items-center gap-2">
                    {controlState !== 'idle' ? (
                      <Loader className="h-3 w-3 animate-spin text-indigo-400" />
                    ) : currentMission.status === 'completed' ? (
                      <CheckCircle className="h-3 w-3 text-emerald-400" />
                    ) : currentMission.status === 'failed' ? (
                      <XCircle className="h-3 w-3 text-red-400" />
                    ) : (
                      <Clock className="h-3 w-3 text-indigo-400" />
                    )}
                    <span className="truncate text-sm text-white/80">
                      {currentMission.title?.slice(0, 25) || 'Current Mission'}
                    </span>
                  </div>
                </button>
              )}
              
              {/* Other missions */}
              {filteredMissions.filter(m => m.id !== currentMission?.id).map((mission) => (
                <button
                  key={mission.id}
                  onClick={() => setSelectedMissionId(mission.id)}
                  className={cn(
                    'w-full rounded-xl p-3 text-left transition-all',
                    selectedMissionId === mission.id
                      ? 'bg-white/[0.08] border border-indigo-500/50'
                      : 'bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04] hover:border-white/[0.08]'
                  )}
                >
                  <div className="flex items-center gap-2">
                    {mission.status === 'active' ? (
                      <Clock className="h-3 w-3 text-indigo-400" />
                    ) : mission.status === 'completed' ? (
                      <CheckCircle className="h-3 w-3 text-emerald-400" />
                    ) : (
                      <XCircle className="h-3 w-3 text-red-400" />
                    )}
                    <span className="truncate text-sm text-white/80">
                      {mission.title?.slice(0, 25) || 'Mission ' + mission.id.slice(0, 8)}
                    </span>
                  </div>
                </button>
              ))}
            </>
          )}
        </div>
      </div>

      {/* Agent tree */}
      <div className="flex-1 overflow-auto p-6">
        <div className="mb-6">
          <h1 className="text-xl font-semibold text-white">Agent Tree</h1>
          <p className="mt-1 text-sm text-white/50">
            Visualize the hierarchical agent structure
          </p>
        </div>

        {loading ? (
          <div className="space-y-4">
            <ShimmerCard />
            <div className="ml-6 space-y-4">
              <ShimmerCard />
              <ShimmerCard />
            </div>
          </div>
        ) : mockAgentTree ? (
          <div className="space-y-2">
            <AgentTreeNode
              agent={mockAgentTree}
              onSelect={setSelectedAgent}
              selectedId={selectedAgent?.id || null}
            />
          </div>
        ) : missions.length === 0 && !currentMission ? (
          <div className="flex flex-col items-center justify-center py-16">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
              <MessageSquare className="h-8 w-8 text-white/30" />
            </div>
            <p className="text-white/80">No active missions</p>
            <p className="mt-2 text-sm text-white/40">
              Start a conversation in the{' '}
              <Link href="/control" className="text-indigo-400 hover:text-indigo-300">
                Control
              </Link>{' '}
              page
            </p>
          </div>
        ) : (
          <div className="flex items-center justify-center py-16">
            <p className="text-white/40">Select a mission to view agent tree</p>
          </div>
        )}
      </div>

      {/* Agent details panel */}
      {selectedAgent && (
        <div className="w-80 border-l border-white/[0.06] glass-panel p-4 animate-slide-in-right">
          <h2 className="mb-4 text-lg font-medium text-white">
            {selectedAgent.name}
          </h2>

          <div className="space-y-4">
            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Type</label>
              <p className="text-sm text-white">{selectedAgent.type}</p>
            </div>

            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Status</label>
              <p className={cn('text-sm capitalize', statusConfig[selectedAgent.status].text)}>
                {selectedAgent.status}
              </p>
            </div>

            <div className="group">
              <label className="text-[10px] uppercase tracking-wider text-white/40">Description</label>
              <div className="flex items-start gap-2">
                <p className="text-sm text-white/80 flex-1">{selectedAgent.description}</p>
                <CopyButton text={selectedAgent.description} showOnHover />
              </div>
            </div>

            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Budget</label>
              <div className="mt-1">
                <div className="flex justify-between text-sm">
                  <span className="text-white tabular-nums">
                    {formatCents(selectedAgent.budgetSpent)}
                  </span>
                  <span className="text-white/40">
                    of {formatCents(selectedAgent.budgetAllocated)}
                  </span>
                </div>
                <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-white/[0.08]">
                  <div
                    className="h-full rounded-full bg-indigo-500 transition-all duration-500"
                    style={{
                      width: `${Math.min(100, (selectedAgent.budgetSpent / selectedAgent.budgetAllocated) * 100)}%`,
                    }}
                  />
                </div>
              </div>
            </div>

            {selectedAgent.complexity !== undefined && (
              <div>
                <label className="text-[10px] uppercase tracking-wider text-white/40">Complexity Score</label>
                <p className="text-sm text-white tabular-nums">
                  {(selectedAgent.complexity * 100).toFixed(0)}%
                </p>
              </div>
            )}

            {selectedAgent.selectedModel && (
              <div className="group">
                <label className="text-[10px] uppercase tracking-wider text-white/40">Selected Model</label>
                <div className="flex items-center gap-2">
                  <p className="text-sm text-white font-mono">{selectedAgent.selectedModel.split('/').pop()}</p>
                  <CopyButton text={selectedAgent.selectedModel} showOnHover />
                </div>
              </div>
            )}

            {selectedAgent.logs && selectedAgent.logs.length > 0 && (
              <div>
                <label className="text-[10px] uppercase tracking-wider text-white/40">
                  Logs ({selectedAgent.logs.length})
                </label>
                <div 
                  ref={logsContainerRef}
                  className="mt-2 max-h-48 space-y-2 overflow-auto"
                >
                  {selectedAgent.logs.map((log, i) => (
                    <div
                      key={i}
                      className="group rounded-lg bg-white/[0.02] border border-white/[0.04] p-2 text-xs font-mono text-white/60"
                    >
                      <div className="flex items-start gap-2">
                        <p className="flex-1">{log.slice(0, 80)}...</p>
                        <CopyButton text={log} showOnHover />
                      </div>
                    </div>
                  ))}
                  <div ref={logsEndRef} />
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
