/**
 * Agent Tree Visualization
 * 
 * Dynamic, animated tree visualization for agent execution.
 * 
 * @example
 * ```tsx
 * import { AgentTreeCanvas, generateComplexTree, simulateTreeUpdates } from '@/components/agent-tree';
 * 
 * // With real data
 * <AgentTreeCanvas tree={agentTree} onSelectNode={setSelectedNode} />
 * 
 * // With demo data (for testing)
 * const [tree, setTree] = useState(generateComplexTree());
 * useEffect(() => simulateTreeUpdates(tree, setTree), []);
 * <AgentTreeCanvas tree={tree} />
 * ```
 */

export { AgentTreeCanvas } from './AgentTreeCanvas';
export { computeLayout, getTreeStats, getAllNodeIds } from './layout';
export { 
  generateSimpleTree, 
  generateComplexTree, 
  generateDeepTree,
  simulateTreeUpdates,
} from './demo-data';
export type { 
  AgentNode, 
  AgentType, 
  AgentStatus, 
  TreeLayout, 
  LayoutNode, 
  LayoutEdge,
  TreeStats,
} from './types';
