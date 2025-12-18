/**
 * Demo Data Generator
 * 
 * Generates fake but realistic agent tree data for testing the visualization
 * without consuming API resources.
 */

import type { AgentNode, AgentStatus, AgentType } from './types';

const MODELS = [
  'claude-3.5-haiku',
  'claude-3.5-sonnet', 
  'claude-sonnet-4.5',
  'gpt-4o-mini',
  'gpt-4o',
  'gemini-2.0-flash',
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

function createLeafAgents(parentId: string, status: AgentStatus): AgentNode[] {
  const isCompleted = status === 'completed';
  const isRunning = status === 'running';
  
  return [
    {
      id: `${parentId}-executor`,
      type: 'TaskExecutor' as AgentType,
      status: isRunning ? 'running' : isCompleted ? 'completed' : 'pending',
      name: 'Task Executor',
      description: 'Execute task using tools',
      model: randomModel(),
      budgetAllocated: 500,
      budgetSpent: isCompleted ? Math.floor(Math.random() * 400 + 50) : isRunning ? Math.floor(Math.random() * 200) : 0,
    },
    {
      id: `${parentId}-verifier`,
      type: 'Verifier' as AgentType,
      status: isCompleted ? 'completed' : 'pending',
      name: 'Verifier',
      description: 'Verify task completion',
      model: randomModel(),
      budgetAllocated: 50,
      budgetSpent: isCompleted ? Math.floor(Math.random() * 30 + 5) : 0,
    },
  ];
}

/**
 * Generate a simple tree with just the orchestrator agents
 */
export function generateSimpleTree(): AgentNode {
  return {
    id: 'root',
    type: 'Root',
    status: 'running',
    name: 'Root Agent',
    description: 'Mission orchestrator',
    model: 'claude-sonnet-4.5',
    budgetAllocated: 1000,
    budgetSpent: 150,
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
        complexity: 0.72,
      },
      {
        id: 'model-selector',
        type: 'ModelSelector',
        status: 'completed',
        name: 'Model Selector',
        description: 'Select optimal model',
        model: 'claude-3.5-haiku',
        budgetAllocated: 10,
        budgetSpent: 3,
      },
      {
        id: 'executor',
        type: 'TaskExecutor',
        status: 'running',
        name: 'Task Executor',
        description: 'Execute main task',
        model: 'claude-sonnet-4.5',
        budgetAllocated: 900,
        budgetSpent: 125,
      },
      {
        id: 'verifier',
        type: 'Verifier',
        status: 'pending',
        name: 'Verifier',
        description: 'Verify task completion',
        model: 'claude-3.5-haiku',
        budgetAllocated: 80,
        budgetSpent: 0,
      },
    ],
  };
}

/**
 * Generate a complex tree with subtasks and nested agents
 */
export function generateComplexTree(): AgentNode {
  const subtasks: AgentNode[] = [];
  const numSubtasks = Math.floor(Math.random() * 3) + 3; // 3-5 subtasks
  
  for (let i = 0; i < numSubtasks; i++) {
    const bias = i < numSubtasks / 3 ? 'late' : i < (numSubtasks * 2) / 3 ? 'middle' : 'early';
    const status = randomStatus(bias);
    
    subtasks.push({
      id: `subtask-${i + 1}`,
      type: 'Node',
      status,
      name: `Subtask ${i + 1}`,
      description: getSubtaskDescription(i),
      model: randomModel(),
      budgetAllocated: Math.floor(800 / numSubtasks),
      budgetSpent: status === 'completed' 
        ? Math.floor(Math.random() * 100 + 30)
        : status === 'running' 
        ? Math.floor(Math.random() * 50)
        : 0,
      complexity: Math.random() * 0.5 + 0.3,
      children: createLeafAgents(`subtask-${i + 1}`, status),
    });
  }

  return {
    id: 'root',
    type: 'Root',
    status: 'running',
    name: 'Root Agent',
    description: 'Build a full-stack web application',
    model: 'claude-sonnet-4.5',
    budgetAllocated: 2000,
    budgetSpent: 450,
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
        complexity: 0.85,
      },
      {
        id: 'model-selector',
        type: 'ModelSelector',
        status: 'completed',
        name: 'Model Selector',
        description: 'Select optimal model',
        model: 'claude-3.5-haiku',
        budgetAllocated: 10,
        budgetSpent: 3,
      },
      ...subtasks,
      {
        id: 'verifier',
        type: 'Verifier',
        status: 'pending',
        name: 'Verifier',
        description: 'Verify full task completion',
        model: 'claude-3.5-sonnet',
        budgetAllocated: 100,
        budgetSpent: 0,
      },
    ],
  };
}

/**
 * Generate a deeply nested tree for stress testing
 */
export function generateDeepTree(depth: number = 4): AgentNode {
  function createNode(level: number, index: number, parentId: string): AgentNode {
    const id = `${parentId}-${index}`;
    const isLeaf = level >= depth;
    const status = randomStatus(level === 1 ? 'late' : level === depth ? 'early' : 'middle');
    
    return {
      id,
      type: isLeaf ? 'TaskExecutor' : 'Node',
      status,
      name: isLeaf ? `Executor ${index}` : `Node ${level}.${index}`,
      description: isLeaf ? 'Execute leaf task' : `Process level ${level}`,
      model: randomModel(),
      budgetAllocated: Math.floor(1000 / Math.pow(2, level)),
      budgetSpent: status === 'completed' ? Math.floor(Math.random() * 50 + 10) : 0,
      complexity: isLeaf ? undefined : Math.random() * 0.6 + 0.2,
      children: isLeaf 
        ? undefined 
        : Array.from({ length: Math.floor(Math.random() * 2) + 2 }, (_, i) => 
            createNode(level + 1, i + 1, id)
          ),
    };
  }

  return {
    id: 'root',
    type: 'Root',
    status: 'running',
    name: 'Root Agent',
    description: 'Complex recursive task decomposition',
    model: 'claude-sonnet-4.5',
    budgetAllocated: 5000,
    budgetSpent: 1200,
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
        complexity: 0.95,
      },
      {
        id: 'model-selector',
        type: 'ModelSelector',
        status: 'completed',
        name: 'Model Selector',
        description: 'Select optimal model',
        model: 'claude-3.5-haiku',
        budgetAllocated: 10,
        budgetSpent: 3,
      },
      createNode(1, 1, 'branch-a'),
      createNode(1, 2, 'branch-b'),
      {
        id: 'verifier',
        type: 'Verifier',
        status: 'pending',
        name: 'Final Verifier',
        description: 'Verify all branches completed',
        model: 'claude-3.5-sonnet',
        budgetAllocated: 150,
        budgetSpent: 0,
      },
    ],
  };
}

function getSubtaskDescription(index: number): string {
  const descriptions = [
    'Set up project structure and dependencies',
    'Implement database schema and migrations',
    'Create API endpoints and routes',
    'Build frontend components',
    'Add authentication and authorization',
    'Write tests and documentation',
    'Configure deployment pipeline',
  ];
  return descriptions[index % descriptions.length];
}

/**
 * Simulate real-time updates to the tree
 */
export function simulateTreeUpdates(
  tree: AgentNode,
  onUpdate: (tree: AgentNode) => void
): () => void {
  let currentTree = JSON.parse(JSON.stringify(tree)) as AgentNode;
  
  const updateNode = (node: AgentNode): boolean => {
    // Progress running nodes
    if (node.status === 'running') {
      node.budgetSpent = Math.min(
        node.budgetAllocated,
        node.budgetSpent + Math.floor(Math.random() * 10 + 5)
      );
      
      // Sometimes complete the node
      if (Math.random() < 0.15) {
        node.status = Math.random() < 0.9 ? 'completed' : 'failed';
        return true;
      }
    }
    
    // Start pending nodes if siblings are done
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
