'use client';

import { useEffect, useMemo, useState, useRef } from 'react';
import Link from 'next/link';
import { cn } from '@/lib/utils';
import { listTasks, listRuns, TaskState, Run } from '@/lib/api';
import { formatCents } from '@/lib/utils';
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

function mapTaskStatusToAgentStatus(status: TaskState['status']): AgentNode['status'] {
  switch (status) {
    case 'running': return 'running';
    case 'completed': return 'completed';
    case 'failed': return 'failed';
    case 'pending': return 'pending';
    case 'cancelled': return 'cancelled';
  }
}

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

export default function AgentsPage() {
  const [tasks, setTasks] = useState<TaskState[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const selectedTask = useMemo(
    () => tasks.find((t) => t.id === selectedTaskId) ?? null,
    [tasks, selectedTaskId]
  );
  const [selectedAgent, setSelectedAgent] = useState<AgentNode | null>(null);
  const [loading, setLoading] = useState(true);
  const fetchedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    let seq = 0;

    const fetchData = async () => {
      const mySeq = ++seq;
      try {
        const [tasksData, runsData] = await Promise.all([
          listTasks().catch(() => []),
          !fetchedRef.current ? listRuns().catch(() => ({ runs: [] })) : Promise.resolve({ runs }),
        ]);
        if (cancelled || mySeq !== seq) return;
        
        fetchedRef.current = true;
        setTasks(tasksData);
        if ('runs' in runsData) {
          setRuns(runsData.runs || []);
        }
        setSelectedTaskId((prev) => {
          if (tasksData.length === 0) return null;
          if (!prev) return tasksData[0]!.id;
          const stillExists = tasksData.some((t) => t.id === prev);
          return stillExists ? prev : tasksData[0]!.id;
        });
      } catch (error) {
        console.error('Failed to fetch data:', error);
      } finally {
        if (!cancelled && mySeq === seq) {
          setLoading(false);
        }
      }
    };

    fetchData();
    const interval = setInterval(fetchData, 3000);
    return () => {
      cancelled = true;
      seq += 1;
      clearInterval(interval);
    };
  }, [runs]);

  const mockAgentTree: AgentNode | null = selectedTask
    ? {
        id: 'root',
        type: 'Root',
        status: mapTaskStatusToAgentStatus(selectedTask.status),
        name: 'Root Agent',
        description: selectedTask.task.slice(0, 50) + '...',
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
            selectedModel: selectedTask.model,
          },
          {
            id: 'executor',
            type: 'TaskExecutor',
            status: mapTaskStatusToAgentStatus(selectedTask.status),
            name: 'Task Executor',
            description: 'Execute using tools',
            budgetAllocated: 900,
            budgetSpent: 35,
            logs: selectedTask.log.map((l) => l.content),
          },
          {
            id: 'verifier',
            type: 'Verifier',
            status:
              selectedTask.status === 'completed'
                ? 'completed'
                : selectedTask.status === 'failed'
                  ? 'failed'
                  : selectedTask.status === 'cancelled'
                    ? 'cancelled'
                    : 'pending',
            name: 'Verifier',
            description: 'Verify task completion',
            budgetAllocated: 80,
            budgetSpent: selectedTask.status === 'completed' ? 7 : 0,
          },
        ],
      }
    : null;

  return (
    <div className="flex h-screen">
      {/* Task selector sidebar */}
      <div className="w-64 border-r border-white/[0.06] glass-panel p-4">
        <h2 className="mb-4 text-sm font-medium text-white">Tasks</h2>
        <div className="space-y-2">
          {tasks.map((task) => (
            <button
              key={task.id}
              onClick={() => setSelectedTaskId(task.id)}
              className={cn(
                'w-full rounded-xl p-3 text-left transition-all',
                selectedTaskId === task.id
                  ? 'bg-white/[0.08] border border-indigo-500/50'
                  : 'bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04] hover:border-white/[0.08]'
              )}
            >
              <div className="flex items-center gap-2">
                {task.status === 'running' && (
                  <Loader className="h-3 w-3 animate-spin text-indigo-400" />
                )}
                {task.status === 'completed' && (
                  <CheckCircle className="h-3 w-3 text-emerald-400" />
                )}
                {task.status === 'failed' && (
                  <XCircle className="h-3 w-3 text-red-400" />
                )}
                {task.status === 'pending' && (
                  <Clock className="h-3 w-3 text-amber-400" />
                )}
                {task.status === 'cancelled' && (
                  <Ban className="h-3 w-3 text-white/40" />
                )}
                <span className="truncate text-sm text-white/80">
                  {task.task.slice(0, 25)}...
                </span>
              </div>
            </button>
          ))}
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
          <div className="flex items-center justify-center py-16">
            <Loader className="h-8 w-8 animate-spin text-indigo-400" />
          </div>
        ) : mockAgentTree ? (
          <div className="space-y-2">
            <AgentTreeNode
              agent={mockAgentTree}
              onSelect={setSelectedAgent}
              selectedId={selectedAgent?.id || null}
            />
          </div>
        ) : tasks.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02] mb-4">
              <MessageSquare className="h-8 w-8 text-white/30" />
            </div>
            <p className="text-white/80">No active tasks</p>
            <p className="mt-2 text-sm text-white/40">
              Start a conversation in the{' '}
              <Link href="/control" className="text-indigo-400 hover:text-indigo-300">
                Control
              </Link>{' '}
              page
            </p>
            {runs.length > 0 && (
              <p className="mt-3 text-xs text-white/30">
                You have {runs.length} archived runs in{' '}
                <Link href="/history" className="text-indigo-400 hover:text-indigo-300">
                  History
                </Link>
              </p>
            )}
          </div>
        ) : (
          <div className="flex items-center justify-center py-16">
            <p className="text-white/40">Select a task to view agent tree</p>
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

            <div>
              <label className="text-[10px] uppercase tracking-wider text-white/40">Description</label>
              <p className="text-sm text-white/80">{selectedAgent.description}</p>
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
                    className="h-full rounded-full bg-indigo-500"
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
              <div>
                <label className="text-[10px] uppercase tracking-wider text-white/40">Selected Model</label>
                <p className="text-sm text-white font-mono">{selectedAgent.selectedModel.split('/').pop()}</p>
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
                      className="rounded-lg bg-white/[0.02] border border-white/[0.04] p-2 text-xs font-mono text-white/60"
                    >
                      {log.slice(0, 80)}...
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
