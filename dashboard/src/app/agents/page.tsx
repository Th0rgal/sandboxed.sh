'use client';

import { useEffect, useMemo, useState, useRef, useCallback } from 'react';
import Link from 'next/link';
import { toast } from 'sonner';
import { cn } from '@/lib/utils';
import { listMissions, getCurrentMission, streamControl, Mission, ControlRunState } from '@/lib/api';
import { formatCents } from '@/lib/utils';
import { ShimmerSidebarItem, ShimmerCard } from '@/components/ui/shimmer';
import { CopyButton } from '@/components/ui/copy-button';
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
  ChevronLeft,
  X,
  Zap,
  GitBranch,
  Target,
  MessageSquare,
  Search,
  Network,
  Layers,
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
  depth?: number;
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
  running: { 
    border: 'border-indigo-500/50', 
    bg: 'bg-indigo-500/10', 
    text: 'text-indigo-400',
    glow: 'shadow-[0_0_20px_rgba(99,102,241,0.3)]',
    line: 'bg-indigo-500',
  },
  completed: { 
    border: 'border-emerald-500/50', 
    bg: 'bg-emerald-500/10', 
    text: 'text-emerald-400',
    glow: '',
    line: 'bg-emerald-500',
  },
  failed: { 
    border: 'border-red-500/50', 
    bg: 'bg-red-500/10', 
    text: 'text-red-400',
    glow: '',
    line: 'bg-red-500',
  },
  pending: { 
    border: 'border-amber-500/30', 
    bg: 'bg-amber-500/5', 
    text: 'text-amber-400',
    glow: '',
    line: 'bg-amber-500/50',
  },
  paused: { 
    border: 'border-white/10', 
    bg: 'bg-white/[0.02]', 
    text: 'text-white/40',
    glow: '',
    line: 'bg-white/20',
  },
  cancelled: { 
    border: 'border-white/10', 
    bg: 'bg-white/[0.02]', 
    text: 'text-white/40',
    glow: '',
    line: 'bg-white/20',
  },
};

// Tree node component with visual connecting lines
function TreeNode({
  agent,
  depth = 0,
  isLast = false,
  parentPath = [],
  onSelect,
  selectedId,
  expandedNodes,
  toggleExpanded,
}: {
  agent: AgentNode;
  depth?: number;
  isLast?: boolean;
  parentPath?: boolean[];
  onSelect: (agent: AgentNode) => void;
  selectedId: string | null;
  expandedNodes: Set<string>;
  toggleExpanded: (id: string) => void;
}) {
  const Icon = agentIcons[agent.type];
  const hasChildren = agent.children && agent.children.length > 0;
  const isExpanded = expandedNodes.has(agent.id);
  const isSelected = selectedId === agent.id;
  const config = statusConfig[agent.status];

  return (
    <div className="relative">
      {/* Tree structure lines */}
      <div className="flex">
        {/* Vertical lines from parent levels */}
        {parentPath.map((hasMore, idx) => (
          <div key={idx} className="relative w-8 flex-shrink-0">
            {hasMore && (
              <div className="absolute left-4 top-0 bottom-0 w-px bg-white/[0.06]" />
            )}
          </div>
        ))}
        
        {/* Current level connector */}
        {depth > 0 && (
          <div className="relative w-8 flex-shrink-0">
            {/* Horizontal line to node */}
            <div className="absolute left-0 top-6 w-4 h-px bg-white/[0.08]" />
            {/* Vertical line to siblings below */}
            {!isLast && (
              <div className="absolute left-0 top-6 bottom-0 w-px bg-white/[0.06]" />
            )}
            {/* Vertical line from parent */}
            <div className="absolute left-0 top-0 h-6 w-px bg-white/[0.06]" />
          </div>
        )}

        {/* Node content */}
        <div className={cn("flex-1 min-w-0", depth > 0 && "pl-0")}>
          <div
            className={cn(
              'group flex cursor-pointer items-center gap-3 rounded-xl border p-3 transition-all duration-200',
              config.border,
              config.bg,
              config.glow,
              isSelected && 'ring-2 ring-indigo-500/50 ring-offset-1 ring-offset-black/50',
              'hover:bg-white/[0.06]'
            )}
            onClick={() => onSelect(agent)}
          >
            {/* Expand/collapse button */}
            {hasChildren ? (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  toggleExpanded(agent.id);
                }}
                className="flex h-6 w-6 items-center justify-center rounded-md hover:bg-white/[0.08] transition-colors"
              >
                <ChevronRight 
                  className={cn(
                    "h-4 w-4 text-white/40 transition-transform duration-200",
                    isExpanded && "rotate-90"
                  )} 
                />
              </button>
            ) : (
              <div className="w-6" />
            )}

            {/* Agent icon */}
            <div className={cn(
              'flex h-9 w-9 items-center justify-center rounded-lg transition-all',
              config.bg,
              agent.status === 'running' && 'animate-pulse'
            )}>
              <Icon className={cn('h-4 w-4', config.text)} />
            </div>

            {/* Agent info */}
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="font-medium text-white text-sm">{agent.name}</span>
                <span className={cn(
                  "px-1.5 py-0.5 rounded text-[9px] font-medium uppercase tracking-wide",
                  config.bg, config.text
                )}>
                  {agent.type}
                </span>
                {agent.complexity !== undefined && (
                  <span className="px-1.5 py-0.5 rounded text-[9px] font-mono bg-white/[0.04] text-white/50">
                    {(agent.complexity * 100).toFixed(0)}%
                  </span>
                )}
              </div>
              <p className="truncate text-xs text-white/40 mt-0.5">{agent.description}</p>
            </div>

            {/* Status and budget */}
            <div className="flex items-center gap-3 flex-shrink-0">
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
                <div className="text-xs font-medium text-white tabular-nums">
                  {formatCents(agent.budgetSpent)}
                </div>
                <div className="text-[10px] text-white/30 tabular-nums">
                  / {formatCents(agent.budgetAllocated)}
                </div>
              </div>
            </div>
          </div>

          {/* Children with animation */}
          {hasChildren && isExpanded && (
            <div className="mt-2 space-y-2 animate-slide-up">
              {agent.children!.map((child, idx) => (
                <TreeNode
                  key={child.id}
                  agent={child}
                  depth={depth + 1}
                  isLast={idx === agent.children!.length - 1}
                  parentPath={[...parentPath, !isLast]}
                  onSelect={onSelect}
                  selectedId={selectedId}
                  expandedNodes={expandedNodes}
                  toggleExpanded={toggleExpanded}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// Visual mini-map for large trees
function TreeMiniMap({ 
  tree, 
  stats 
}: { 
  tree: AgentNode | null; 
  stats: { total: number; running: number; completed: number; failed: number } 
}) {
  if (!tree) return null;
  
  return (
    <div className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.04]">
      <div className="flex items-center gap-2 mb-3">
        <Network className="h-4 w-4 text-white/40" />
        <span className="text-xs font-medium text-white/60">Tree Overview</span>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <div>
          <div className="text-[10px] uppercase tracking-wider text-white/30">Total Agents</div>
          <div className="text-lg font-light text-white tabular-nums">{stats.total}</div>
        </div>
        <div>
          <div className="text-[10px] uppercase tracking-wider text-emerald-400/60">Completed</div>
          <div className="text-lg font-light text-emerald-400 tabular-nums">{stats.completed}</div>
        </div>
        <div>
          <div className="text-[10px] uppercase tracking-wider text-indigo-400/60">Running</div>
          <div className="text-lg font-light text-indigo-400 tabular-nums">{stats.running}</div>
        </div>
        <div>
          <div className="text-[10px] uppercase tracking-wider text-red-400/60">Failed</div>
          <div className="text-lg font-light text-red-400 tabular-nums">{stats.failed}</div>
        </div>
      </div>
    </div>
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

// Recursive function to count nodes by status
function countNodes(node: AgentNode | null): { total: number; running: number; completed: number; failed: number } {
  if (!node) return { total: 0, running: 0, completed: 0, failed: 0 };
  
  let stats = {
    total: 1,
    running: node.status === 'running' ? 1 : 0,
    completed: node.status === 'completed' ? 1 : 0,
    failed: node.status === 'failed' ? 1 : 0,
  };
  
  if (node.children) {
    for (const child of node.children) {
      const childStats = countNodes(child);
      stats.total += childStats.total;
      stats.running += childStats.running;
      stats.completed += childStats.completed;
      stats.failed += childStats.failed;
    }
  }
  
  return stats;
}

// Get all node IDs for expanding
function getAllNodeIds(node: AgentNode | null, ids: Set<string> = new Set()): Set<string> {
  if (!node) return ids;
  ids.add(node.id);
  if (node.children) {
    for (const child of node.children) {
      getAllNodeIds(child, ids);
    }
  }
  return ids;
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
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set(['root']));
  const [realTree, setRealTree] = useState<AgentNode | null>(null);
  const fetchedRef = useRef(false);
  const streamCleanupRef = useRef<null | (() => void)>(null);

  const toggleExpanded = useCallback((id: string) => {
    setExpandedNodes(prev => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const expandAll = useCallback((tree: AgentNode | null) => {
    setExpandedNodes(getAllNodeIds(tree));
  }, []);

  const collapseAll = useCallback(() => {
    setExpandedNodes(new Set(['root']));
  }, []);

  // Convert backend tree node to frontend AgentNode
  const convertTreeNode = useCallback((node: Record<string, unknown>): AgentNode => {
    const children = (node['children'] as Record<string, unknown>[] | undefined) ?? [];
    return {
      id: String(node['id'] ?? ''),
      type: (String(node['node_type'] ?? 'Node') as AgentNode['type']),
      status: (String(node['status'] ?? 'pending') as AgentNode['status']),
      name: String(node['name'] ?? ''),
      description: String(node['description'] ?? ''),
      budgetAllocated: Number(node['budget_allocated'] ?? 0),
      budgetSpent: Number(node['budget_spent'] ?? 0),
      complexity: node['complexity'] != null ? Number(node['complexity']) : undefined,
      selectedModel: node['selected_model'] != null ? String(node['selected_model']) : undefined,
      children: children.map((c) => convertTreeNode(c)),
    };
  }, []);

  // Stream control events for real-time status and tree updates
  useEffect(() => {
    streamCleanupRef.current?.();

    const cleanup = streamControl((event) => {
      const data: unknown = event.data;
      if (event.type === 'status' && isRecord(data)) {
        const st = data['state'];
        setControlState(typeof st === 'string' ? (st as ControlRunState) : 'idle');
        
        // Clear real tree when idle
        if (st === 'idle') {
          setRealTree(null);
        }
      }
      
      // Handle real-time tree updates
      if (event.type === 'agent_tree' && isRecord(data)) {
        const tree = data['tree'];
        if (isRecord(tree)) {
          const converted = convertTreeNode(tree);
          setRealTree(converted);
          
          // Auto-expand new nodes
          const getAllIds = (node: AgentNode): string[] => {
            const ids = [node.id];
            if (node.children) {
              for (const child of node.children) {
                ids.push(...getAllIds(child));
              }
            }
            return ids;
          };
          setExpandedNodes((prev) => {
            const next = new Set(prev);
            for (const id of getAllIds(converted)) {
              next.add(id);
            }
            return next;
          });
        }
      }
    });

    streamCleanupRef.current = cleanup;
    return () => {
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
    };
  }, [convertTreeNode]);

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

  const filteredMissions = useMemo(() => {
    if (!searchQuery.trim()) return missions;
    const query = searchQuery.toLowerCase();
    return missions.filter((m) => 
      m.title?.toLowerCase().includes(query) || 
      m.id.toLowerCase().includes(query)
    );
  }, [missions, searchQuery]);

  const controlStateToStatus = (state: ControlRunState, missionStatus?: string): AgentNode['status'] => {
    if (state === 'running' || state === 'waiting_for_tool') return 'running';
    if (missionStatus === 'completed') return 'completed';
    if (missionStatus === 'failed') return 'failed';
    return 'pending';
  };

  // Build a more realistic agent tree from mission data
  const buildAgentTree = useCallback((): AgentNode | null => {
    if (!selectedMission) return null;

    const rootStatus = controlStateToStatus(controlState, selectedMission.status);
    
    // Create subtask nodes from mission history
    const subtaskNodes: AgentNode[] = [];
    let subtaskIdx = 0;
    
    for (const entry of selectedMission.history) {
      if (entry.role === 'assistant' && entry.content.includes('subtask')) {
        subtaskIdx++;
        subtaskNodes.push({
          id: `subtask-${subtaskIdx}`,
          type: 'Node',
          status: rootStatus === 'running' ? (subtaskIdx === subtaskNodes.length ? 'running' : 'completed') : rootStatus,
          name: `Subtask ${subtaskIdx}`,
          description: entry.content.slice(0, 60) + '...',
          budgetAllocated: Math.floor(800 / Math.max(1, selectedMission.history.filter(h => h.content.includes('subtask')).length)),
          budgetSpent: Math.floor(Math.random() * 20 + 5),
          complexity: Math.random() * 0.4 + 0.3,
          children: [
            {
              id: `subtask-${subtaskIdx}-executor`,
              type: 'TaskExecutor',
              status: rootStatus === 'running' ? 'running' : rootStatus,
              name: 'Task Executor',
              description: 'Execute subtask using tools',
              budgetAllocated: 100,
              budgetSpent: 15,
            },
            {
              id: `subtask-${subtaskIdx}-verifier`,
              type: 'Verifier',
              status: rootStatus === 'completed' ? 'completed' : 'pending',
              name: 'Verifier',
              description: 'Verify subtask completion',
              budgetAllocated: 20,
              budgetSpent: rootStatus === 'completed' ? 5 : 0,
            },
          ],
        });
      }
    }

    return {
      id: 'root',
      type: 'Root',
      status: rootStatus,
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
          complexity: 0.7,
        },
        {
          id: 'model-selector',
          type: 'ModelSelector',
          status: 'completed',
          name: 'Model Selector',
          description: 'Select optimal model for task',
          budgetAllocated: 10,
          budgetSpent: 3,
          selectedModel: 'claude-sonnet-4.5',
        },
        ...(subtaskNodes.length > 0 ? subtaskNodes : [
          {
            id: 'executor',
            type: 'TaskExecutor',
            status: rootStatus,
            name: 'Task Executor',
            description: 'Execute task using tools',
            budgetAllocated: 900,
            budgetSpent: 35,
            logs: selectedMission.history.slice(-5).map((h) => h.content.slice(0, 100)),
          },
        ]),
        {
          id: 'verifier',
          type: 'Verifier',
          status: selectedMission.status === 'completed' ? 'completed' : 
                  selectedMission.status === 'failed' ? 'failed' : 'pending',
          name: 'Verifier',
          description: 'Verify task completion',
          budgetAllocated: 80,
          budgetSpent: selectedMission.status === 'completed' ? 7 : 0,
        },
      ] as AgentNode[],
    };
  }, [selectedMission, controlState]);

  // Use real tree when available, fall back to mock tree
  const agentTree = useMemo(() => realTree ?? buildAgentTree(), [realTree, buildAgentTree]);
  const treeStats = useMemo(() => countNodes(agentTree), [agentTree]);
  const isActive = controlState !== 'idle';

  return (
    <div className="flex h-screen">
      {/* Mission selector sidebar */}
      <div className="w-64 border-r border-white/[0.06] glass-panel p-4 flex flex-col">
        <h2 className="mb-3 text-sm font-medium text-white">Missions</h2>
        
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

        {/* Tree mini-map at bottom of sidebar */}
        {agentTree && (
          <div className="mt-4 pt-4 border-t border-white/[0.06]">
            <TreeMiniMap tree={agentTree} stats={treeStats} />
          </div>
        )}
      </div>

      {/* Agent tree */}
      <div className="flex-1 overflow-auto p-6">
        <div className="mb-6 flex items-center justify-between">
          <div>
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Layers className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h1 className="text-xl font-semibold text-white">Agent Tree</h1>
                <p className="text-sm text-white/50">
                  Hierarchical agent execution visualization
                </p>
              </div>
            </div>
          </div>

          {agentTree && (
            <div className="flex items-center gap-2">
              <button
                onClick={() => expandAll(agentTree)}
                className="px-3 py-1.5 rounded-lg text-xs font-medium text-white/60 hover:text-white hover:bg-white/[0.04] transition-colors"
              >
                Expand All
              </button>
              <button
                onClick={collapseAll}
                className="px-3 py-1.5 rounded-lg text-xs font-medium text-white/60 hover:text-white hover:bg-white/[0.04] transition-colors"
              >
                Collapse All
              </button>
            </div>
          )}
        </div>

        {loading ? (
          <div className="space-y-4">
            <ShimmerCard />
            <div className="ml-10 space-y-3">
              <ShimmerCard />
              <ShimmerCard />
              <ShimmerCard />
            </div>
          </div>
        ) : agentTree ? (
          <div className="space-y-2">
            <TreeNode
              agent={agentTree}
              onSelect={setSelectedAgent}
              selectedId={selectedAgent?.id || null}
              expandedNodes={expandedNodes}
              toggleExpanded={toggleExpanded}
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
              page to see the agent tree
            </p>
          </div>
        ) : (
          <div className="flex items-center justify-center py-16">
            <p className="text-white/40">Select a mission to view agent tree</p>
          </div>
        )}
      </div>

      {/* Agent details panel (overlay) */}
      {selectedAgent && (
        <div className="fixed right-0 top-0 z-50 h-full w-80 flex flex-col glass-panel border-l border-white/[0.06] animate-slide-in-right">
          {/* Header */}
          <div className="flex items-start justify-between border-b border-white/[0.06] p-4">
            <div className="flex items-center gap-3">
              <button
                onClick={() => setSelectedAgent(null)}
                className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
              >
                <ChevronLeft className="h-5 w-5" />
              </button>
              <div className={cn(
                'flex h-10 w-10 items-center justify-center rounded-xl',
                statusConfig[selectedAgent.status].bg
              )}>
                {(() => {
                  const Icon = agentIcons[selectedAgent.type];
                  return <Icon className={cn('h-5 w-5', statusConfig[selectedAgent.status].text)} />;
                })()}
              </div>
              <div>
                <h2 className="text-lg font-medium text-white">{selectedAgent.name}</h2>
                <p className={cn('text-xs capitalize', statusConfig[selectedAgent.status].text)}>
                  {selectedAgent.status}
                </p>
              </div>
            </div>
            <button
              onClick={() => setSelectedAgent(null)}
              className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
            >
              <X className="h-5 w-5" />
            </button>
          </div>
          
          {/* Content */}
          <div className="flex-1 overflow-y-auto p-4">

          <div className="space-y-5">
            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Type</label>
              <p className="text-sm text-white mt-1">{selectedAgent.type}</p>
            </div>

            <div className="group">
              <label className="text-[10px] uppercase tracking-wider text-white/40">Description</label>
              <div className="flex items-start gap-2 mt-1">
                <p className="text-sm text-white/80 flex-1">{selectedAgent.description}</p>
                <CopyButton text={selectedAgent.description} showOnHover />
              </div>
            </div>

            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Budget</label>
              <div className="mt-2">
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
                    className={cn(
                      "h-full rounded-full transition-all duration-500",
                      statusConfig[selectedAgent.status].line
                    )}
                    style={{
                      width: `${Math.min(100, (selectedAgent.budgetSpent / selectedAgent.budgetAllocated) * 100)}%`,
                    }}
                  />
                </div>
              </div>
            </div>

            {selectedAgent.complexity !== undefined && (
              <div>
                <label className="text-[10px] uppercase tracking-wider text-white/40">Complexity</label>
                <div className="mt-2">
                  <div className="flex items-center gap-2">
                    <div className="flex-1 h-1.5 overflow-hidden rounded-full bg-white/[0.08]">
                      <div
                        className="h-full rounded-full bg-amber-500 transition-all duration-500"
                        style={{ width: `${selectedAgent.complexity * 100}%` }}
                      />
                    </div>
                    <span className="text-sm text-white tabular-nums">
                      {(selectedAgent.complexity * 100).toFixed(0)}%
                    </span>
                  </div>
                </div>
              </div>
            )}

            {selectedAgent.selectedModel && (
              <div className="group">
                <label className="text-[10px] uppercase tracking-wider text-white/40">Selected Model</label>
                <div className="flex items-center gap-2 mt-1">
                  <div className="px-2 py-1 rounded-md bg-white/[0.04] text-sm font-mono text-white">
                    {selectedAgent.selectedModel.split('/').pop()}
                  </div>
                  <CopyButton text={selectedAgent.selectedModel} showOnHover />
                </div>
              </div>
            )}

            {selectedAgent.logs && selectedAgent.logs.length > 0 && (
              <div>
                <label className="text-[10px] uppercase tracking-wider text-white/40">
                  Logs ({selectedAgent.logs.length})
                </label>
                <div className="mt-2 max-h-48 space-y-2 overflow-auto">
                  {selectedAgent.logs.map((log, i) => (
                    <div
                      key={i}
                      className="group rounded-lg bg-white/[0.02] border border-white/[0.04] p-2 text-xs font-mono text-white/60"
                    >
                      <div className="flex items-start gap-2">
                        <p className="flex-1 break-all">{log.slice(0, 80)}...</p>
                        <CopyButton text={log} showOnHover />
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
