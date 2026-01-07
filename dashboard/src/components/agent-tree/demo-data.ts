/**
 * Demo Data Generator
 *
 * Generates fake but realistic OpenCode session trees for testing the
 * visualization without consuming API resources.
 */

import type { AgentNode, AgentStatus } from './types';

const MODELS = [
  'claude-3.5-haiku',
  'claude-3.5-sonnet',
  'claude-sonnet-4.5',
  'gpt-4o-mini',
  'gpt-4o',
  'gemini-2.0-flash',
];

const PHASES = [
  { id: 'bootstrap', name: 'Bootstrap', description: 'Prepare workspace + config' },
  { id: 'execute', name: 'Execution', description: 'Run tools and commands' },
  { id: 'review', name: 'Review', description: 'Summarize results' },
];

function randomModel(): string {
  return MODELS[Math.floor(Math.random() * MODELS.length)];
}

function randomStatus(bias: 'early' | 'middle' | 'late' = 'middle'): AgentStatus {
  const rand = Math.random();
  switch (bias) {
    case 'early':
      if (rand < 0.6) return 'pending';
      if (rand < 0.9) return 'running';
      return 'completed';
    case 'middle':
      if (rand < 0.3) return 'completed';
      if (rand < 0.6) return 'running';
      if (rand < 0.8) return 'pending';
      return 'failed';
    case 'late':
      if (rand < 0.7) return 'completed';
      if (rand < 0.85) return 'running';
      if (rand < 0.95) return 'pending';
      return 'failed';
  }
}

function createSessionNode({
  id,
  status,
  name,
  description,
  budgetAllocated,
  budgetSpent,
  complexity,
  children,
}: {
  id: string;
  status: AgentStatus;
  name: string;
  description: string;
  budgetAllocated: number;
  budgetSpent: number;
  complexity?: number;
  children?: AgentNode[];
}): AgentNode {
  return {
    id,
    type: 'OpenCodeSession',
    status,
    name,
    description,
    model: randomModel(),
    budgetAllocated,
    budgetSpent,
    complexity,
    children,
  };
}

/**
 * Generate a simple tree with a single OpenCode session.
 */
export function generateSimpleTree(): AgentNode {
  const status: AgentStatus = 'running';
  const budgetAllocated = 1000;
  const budgetSpent = 120;

  return {
    id: 'root',
    type: 'OpenCode',
    status,
    name: 'OpenCode Agent',
    description: 'Delegating mission to OpenCode',
    model: randomModel(),
    budgetAllocated,
    budgetSpent,
    children: [
      createSessionNode({
        id: 'session',
        status,
        name: 'OpenCode Session',
        description: 'Executing mission in workspace',
        budgetAllocated,
        budgetSpent,
        complexity: 0.6,
      }),
    ],
  };
}

/**
 * Generate a richer tree with multiple phases and nested sessions.
 */
export function generateComplexTree(): AgentNode {
  const phaseNodes = PHASES.map((phase, index) => {
    const bias = index === 0 ? 'late' : index === 1 ? 'middle' : 'early';
    const status = randomStatus(bias);
    const budgetAllocated = Math.floor(1200 / PHASES.length);
    const budgetSpent =
      status === 'completed'
        ? Math.floor(Math.random() * 120 + 40)
        : status === 'running'
        ? Math.floor(Math.random() * 80 + 20)
        : 0;

    const children =
      status === 'running'
        ? [
            createSessionNode({
              id: `${phase.id}-tools`,
              status: 'running',
              name: 'Tool Runs',
              description: 'Active tool execution',
              budgetAllocated: Math.floor(budgetAllocated / 2),
              budgetSpent: Math.floor(budgetSpent / 2),
              complexity: 0.4,
            }),
          ]
        : undefined;

    return createSessionNode({
      id: phase.id,
      status,
      name: phase.name,
      description: phase.description,
      budgetAllocated,
      budgetSpent,
      complexity: Math.random() * 0.5 + 0.3,
      children,
    });
  });

  return {
    id: 'root',
    type: 'OpenCode',
    status: 'running',
    name: 'OpenCode Agent',
    description: 'Multi-phase mission execution',
    model: randomModel(),
    budgetAllocated: 2000,
    budgetSpent: 420,
    children: phaseNodes,
  };
}

/**
 * Generate a deeply nested tree for stress testing.
 */
export function generateDeepTree(depth: number = 4): AgentNode {
  function createNode(level: number, index: number, parentId: string): AgentNode {
    const id = `${parentId}-${index}`;
    const isLeaf = level >= depth;
    const status = randomStatus(level === 1 ? 'late' : level === depth ? 'early' : 'middle');

    return createSessionNode({
      id,
      status,
      name: isLeaf ? `Session ${index}` : `Phase ${level}.${index}`,
      description: isLeaf ? 'Execute tools in workspace' : 'Coordinate nested session',
      budgetAllocated: Math.floor(1200 / Math.pow(1.5, level)),
      budgetSpent: status === 'completed' ? Math.floor(Math.random() * 60 + 10) : 0,
      complexity: isLeaf ? undefined : Math.random() * 0.6 + 0.2,
      children: isLeaf
        ? undefined
        : Array.from({ length: Math.floor(Math.random() * 2) + 2 }, (_, i) =>
            createNode(level + 1, i + 1, id)
          ),
    });
  }

  return {
    id: 'root',
    type: 'OpenCode',
    status: 'running',
    name: 'OpenCode Agent',
    description: 'Nested session stress test',
    model: randomModel(),
    budgetAllocated: 5000,
    budgetSpent: 1200,
    children: [createNode(1, 1, 'branch-a'), createNode(1, 2, 'branch-b')],
  };
}

/**
 * Simulate real-time updates to the tree.
 */
export function simulateTreeUpdates(
  tree: AgentNode,
  onUpdate: (tree: AgentNode) => void
): () => void {
  let currentTree = JSON.parse(JSON.stringify(tree)) as AgentNode;

  const updateNode = (node: AgentNode): boolean => {
    if (node.status === 'running') {
      node.budgetSpent = Math.min(
        node.budgetAllocated,
        node.budgetSpent + Math.floor(Math.random() * 10 + 5)
      );

      if (Math.random() < 0.15) {
        node.status = Math.random() < 0.9 ? 'completed' : 'failed';
        return true;
      }
    }

    if (node.status === 'pending' && Math.random() < 0.1) {
      node.status = 'running';
      return true;
    }

    return false;
  };

  const walkTree = (node: AgentNode): boolean => {
    let changed = updateNode(node);
    if (node.children) {
      for (const child of node.children) {
        if (walkTree(child)) changed = true;
      }
    }
    return changed;
  };

  const interval = setInterval(() => {
    if (walkTree(currentTree)) {
      currentTree = JSON.parse(JSON.stringify(currentTree));
      onUpdate(currentTree);
    }
  }, 1000);

  return () => clearInterval(interval);
}
