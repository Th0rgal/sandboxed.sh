'use client';

/**
 * Agent Tree Page
 * 
 * Dynamic visualization of the hierarchical agent execution tree.
 * Shows real-time updates as agents are created, run, and complete.
 */

import { useEffect, useMemo, useState, useRef, useCallback } from 'react';
import Link from 'next/link';
import { toast } from 'sonner';
import { cn } from '@/lib/utils';
import { listMissions, getCurrentMission, streamControl, Mission, ControlRunState } from '@/lib/api';
import { ShimmerSidebarItem } from '@/components/ui/shimmer';
import {
  AgentTreeCanvas,
  generateSimpleTree,
  generateComplexTree,
  generateDeepTree,
  simulateTreeUpdates,
  type AgentNode,
} from '@/components/agent-tree';
import {
  Bot,
  CheckCircle,
  XCircle,
  Loader,
  Clock,
  Search,
  Layers,
  FlaskConical,
  Play,
  Pause,
  MessageSquare,
} from 'lucide-react';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

type DemoMode = 'off' | 'simple' | 'complex' | 'deep';

export default function AgentsPage() {
  const [missions, setMissions] = useState<Mission[]>([]);
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [controlState, setControlState] = useState<ControlRunState>('idle');
  const [selectedMissionId, setSelectedMissionId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [realTree, setRealTree] = useState<AgentNode | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  
  // Demo mode state
  const [demoMode, setDemoMode] = useState<DemoMode>('off');
  const [demoTree, setDemoTree] = useState<AgentNode | null>(null);
  const [demoRunning, setDemoRunning] = useState(false);
  const demoCleanupRef = useRef<(() => void) | null>(null);
  
  const fetchedRef = useRef(false);
  const streamCleanupRef = useRef<null | (() => void)>(null);

  const selectedMission = useMemo(
    () => missions.find((m) => m.id === selectedMissionId) ?? currentMission,
    [missions, selectedMissionId, currentMission]
  );

  // Convert backend tree node to frontend AgentNode
  const convertTreeNode = useCallback((node: Record<string, unknown>): AgentNode => {
    const children = (node['children'] as Record<string, unknown>[] | undefined) ?? [];
    return {
      id: String(node['id'] ?? ''),
      type: (String(node['node_type'] ?? 'Node') as AgentNode['type']),
      status: (String(node['status'] ?? 'pending') as AgentNode['status']),
      name: String(node['name'] ?? ''),
      description: String(node['description'] ?? ''),
      model: node['selected_model'] != null ? String(node['selected_model']) : undefined,
      budgetAllocated: Number(node['budget_allocated'] ?? 0),
      budgetSpent: Number(node['budget_spent'] ?? 0),
      complexity: node['complexity'] != null ? Number(node['complexity']) : undefined,
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
    const interval = setInterval(fetchData, 5000);
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

  // Build a basic agent tree from mission data when no real tree is available
  const buildFallbackTree = useCallback((): AgentNode | null => {
    if (!selectedMission) return null;

    const rootStatus = controlStateToStatus(controlState, selectedMission.status);
    
    return {
      id: 'root',
      type: 'Root',
      status: rootStatus,
      name: 'Root Agent',
      description: selectedMission.title?.slice(0, 50) || 'Mission ' + selectedMission.id.slice(0, 8),
      model: 'claude-sonnet-4.5',
      budgetAllocated: 1000,
      budgetSpent: 50,
      children: [
        {
          id: 'complexity',
          type: 'ComplexityEstimator',
          status: 'completed',
          name: 'Complexity Estimator',
          description: 'Estimate task difficulty',
          model: 'claude-3.5-haiku',
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
          model: 'claude-3.5-haiku',
          budgetAllocated: 10,
          budgetSpent: 3,
        },
        {
          id: 'executor',
          type: 'TaskExecutor',
          status: rootStatus,
          name: 'Task Executor',
          description: 'Execute task using tools',
          model: 'claude-sonnet-4.5',
          budgetAllocated: 900,
          budgetSpent: 35,
        },
        {
          id: 'verifier',
          type: 'Verifier',
          status: selectedMission.status === 'completed' ? 'completed' : 
                  selectedMission.status === 'failed' ? 'failed' : 'pending',
          name: 'Verifier',
          description: 'Verify task completion',
          model: 'claude-3.5-haiku',
          budgetAllocated: 80,
          budgetSpent: selectedMission.status === 'completed' ? 7 : 0,
        },
      ] as AgentNode[],
    };
  }, [selectedMission, controlState]);

  // Demo mode handlers
  const startDemo = useCallback((mode: DemoMode) => {
    // Cleanup previous demo
    demoCleanupRef.current?.();
    
    if (mode === 'off') {
      setDemoMode('off');
      setDemoTree(null);
      setDemoRunning(false);
      return;
    }
    
    // Generate demo tree
    let tree: AgentNode;
    switch (mode) {
      case 'simple':
        tree = generateSimpleTree();
        break;
      case 'complex':
        tree = generateComplexTree();
        break;
      case 'deep':
        tree = generateDeepTree(4);
        break;
      default:
        return;
    }
    
    setDemoMode(mode);
    setDemoTree(tree);
    setDemoRunning(true);
    
    // Start simulation
    const cleanup = simulateTreeUpdates(tree, setDemoTree);
    demoCleanupRef.current = cleanup;
  }, []);
  
  const toggleDemoRunning = useCallback(() => {
    if (demoRunning) {
      demoCleanupRef.current?.();
      demoCleanupRef.current = null;
      setDemoRunning(false);
    } else if (demoTree) {
      const cleanup = simulateTreeUpdates(demoTree, setDemoTree);
      demoCleanupRef.current = cleanup;
      setDemoRunning(true);
    }
  }, [demoRunning, demoTree]);
  
  // Cleanup on unmount
  useEffect(() => {
    return () => {
      demoCleanupRef.current?.();
    };
  }, []);

  // Use demo tree when in demo mode, otherwise use real tree or fallback
  const displayTree = useMemo(() => {
    if (demoMode !== 'off' && demoTree) {
      return demoTree;
    }
    return realTree ?? buildFallbackTree();
  }, [demoMode, demoTree, realTree, buildFallbackTree]);

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
                  onClick={() => {
                    setSelectedMissionId(currentMission.id);
                    if (demoMode !== 'off') startDemo('off');
                  }}
                  className={cn(
                    'w-full rounded-xl p-3 text-left transition-all',
                    selectedMissionId === currentMission.id && demoMode === 'off'
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
                  onClick={() => {
                    setSelectedMissionId(mission.id);
                    if (demoMode !== 'off') startDemo('off');
                  }}
                  className={cn(
                    'w-full rounded-xl p-3 text-left transition-all',
                    selectedMissionId === mission.id && demoMode === 'off'
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

        {/* Demo mode controls */}
        <div className="mt-4 pt-4 border-t border-white/[0.06]">
          <div className="flex items-center gap-2 mb-3">
            <FlaskConical className="h-4 w-4 text-amber-400" />
            <span className="text-xs font-medium text-white/60">Demo Mode</span>
          </div>
          
          <div className="space-y-2">
            <div className="flex gap-1.5">
              {(['simple', 'complex', 'deep'] as const).map((mode) => (
                <button
                  key={mode}
                  onClick={() => startDemo(mode)}
                  className={cn(
                    'flex-1 px-2 py-1.5 rounded-lg text-xs font-medium transition-all capitalize',
                    demoMode === mode
                      ? 'bg-amber-500/20 text-amber-400 border border-amber-500/30'
                      : 'bg-white/[0.02] text-white/50 border border-white/[0.04] hover:bg-white/[0.04]'
                  )}
                >
                  {mode}
                </button>
              ))}
            </div>
            
            {demoMode !== 'off' && (
              <div className="flex gap-2">
                <button
                  onClick={toggleDemoRunning}
                  className={cn(
                    'flex-1 flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-all',
                    demoRunning
                      ? 'bg-amber-500/20 text-amber-400 border border-amber-500/30'
                      : 'bg-white/[0.02] text-white/50 border border-white/[0.04]'
                  )}
                >
                  {demoRunning ? (
                    <>
                      <Pause className="h-3 w-3" />
                      Pause
                    </>
                  ) : (
                    <>
                      <Play className="h-3 w-3" />
                      Resume
                    </>
                  )}
                </button>
                <button
                  onClick={() => startDemo('off')}
                  className="px-3 py-1.5 rounded-lg text-xs font-medium bg-white/[0.02] text-white/50 border border-white/[0.04] hover:bg-white/[0.04] transition-all"
                >
                  Stop
                </button>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Agent tree visualization */}
      <div className="flex-1 flex flex-col overflow-hidden">
        {/* Header */}
        <div className="shrink-0 p-6 pb-0">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
              <Layers className="h-5 w-5 text-indigo-400" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h1 className="text-xl font-semibold text-white">Agent Tree</h1>
                {demoMode !== 'off' && (
                  <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-amber-500/20 text-amber-400 border border-amber-500/30">
                    Demo: {demoMode}
                  </span>
                )}
              </div>
              <p className="text-sm text-white/50">
                {demoMode !== 'off' 
                  ? 'Simulated agent tree with live updates'
                  : 'Hierarchical agent execution visualization'
                }
              </p>
            </div>
          </div>
        </div>

        {/* Tree canvas */}
        <div className="flex-1 p-6 min-h-0">
          {!displayTree && (missions.length === 0 && !currentMission) ? (
            <div className="flex flex-col items-center justify-center h-full">
              <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
                <MessageSquare className="h-8 w-8 text-white/30" />
              </div>
              <p className="text-white/80">No active missions</p>
              <p className="mt-2 text-sm text-white/40 text-center max-w-sm">
                Start a conversation in the{' '}
                <Link href="/control" className="text-indigo-400 hover:text-indigo-300">
                  Control
                </Link>{' '}
                page or try <span className="text-amber-400">Demo Mode</span> in the sidebar
              </p>
            </div>
          ) : (
            <AgentTreeCanvas
              tree={displayTree}
              selectedNodeId={selectedNodeId}
              onSelectNode={(node) => setSelectedNodeId(node?.id ?? null)}
              className="w-full h-full rounded-2xl border border-white/[0.06]"
            />
          )}
        </div>
      </div>
    </div>
  );
}
