/**
 * Tree Layout Algorithm
 * 
 * Computes positions for nodes in a tree structure using a modified
 * Reingold-Tilford algorithm for aesthetic tree layouts.
 */

import type { AgentNode, LayoutNode, LayoutEdge, TreeLayout } from './types';

const NODE_WIDTH = 140;
const NODE_HEIGHT = 80;
const HORIZONTAL_GAP = 40;
const VERTICAL_GAP = 100;

interface LayoutContext {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  nextX: number;
}

/**
 * Compute tree layout with proper spacing and centering
 */
export function computeLayout(root: AgentNode | null): TreeLayout {
  if (!root) {
    return { nodes: [], edges: [], width: 0, height: 0 };
  }

  const ctx: LayoutContext = {
    nodes: [],
    edges: [],
    nextX: 0,
  };

  // First pass: assign x positions based on leaf positions
  assignPositions(root, 0, ctx);

  // Compute bounds
  let minX = Infinity, maxX = -Infinity, maxY = 0;
  for (const node of ctx.nodes) {
    minX = Math.min(minX, node.x);
    maxX = Math.max(maxX, node.x + NODE_WIDTH);
    maxY = Math.max(maxY, node.y + NODE_HEIGHT);
  }

  // Normalize positions (shift to start at 0)
  const offsetX = -minX + 50; // 50px padding
  for (const node of ctx.nodes) {
    node.x += offsetX;
  }
  for (const edge of ctx.edges) {
    edge.from.x += offsetX;
    edge.to.x += offsetX;
  }

  return {
    nodes: ctx.nodes,
    edges: ctx.edges,
    width: maxX - minX + 100,
    height: maxY + 50,
  };
}

function assignPositions(
  node: AgentNode,
  depth: number,
  ctx: LayoutContext,
  parent?: { x: number; y: number }
): number {
  const y = depth * (NODE_HEIGHT + VERTICAL_GAP) + 50;
  
  let x: number;
  
  if (!node.children || node.children.length === 0) {
    // Leaf node - assign next available x
    x = ctx.nextX;
    ctx.nextX += NODE_WIDTH + HORIZONTAL_GAP;
  } else {
    // Internal node - position at center of children
    let childXs: number[] = [];
    
    for (const child of node.children) {
      const childX = assignPositions(child, depth + 1, ctx, { x: 0, y });
      childXs.push(childX);
    }
    
    // Center over children
    x = (Math.min(...childXs) + Math.max(...childXs)) / 2;
  }

  const layoutNode: LayoutNode = {
    id: node.id,
    x,
    y,
    agent: node,
  };
  ctx.nodes.push(layoutNode);

  // Create edge from parent
  if (parent) {
    ctx.edges.push({
      id: `edge-${parent.x}-${parent.y}-${node.id}`,
      from: { x: parent.x + NODE_WIDTH / 2, y: parent.y + NODE_HEIGHT },
      to: { x: x + NODE_WIDTH / 2, y },
      status: node.status,
    });
  }

  // Update edges to use correct parent position
  if (node.children) {
    for (const child of node.children) {
      const edge = ctx.edges.find(e => e.to.x === (ctx.nodes.find(n => n.id === child.id)?.x ?? 0) + NODE_WIDTH / 2);
      if (edge) {
        edge.from = { x: x + NODE_WIDTH / 2, y: y + NODE_HEIGHT };
      }
    }
  }

  return x;
}

/**
 * Get count statistics for a tree
 */
export function getTreeStats(root: AgentNode | null): { 
  total: number; 
  running: number; 
  completed: number; 
  failed: number;
  pending: number;
} {
  if (!root) return { total: 0, running: 0, completed: 0, failed: 0, pending: 0 };
  
  let stats = {
    total: 1,
    running: root.status === 'running' ? 1 : 0,
    completed: root.status === 'completed' ? 1 : 0,
    failed: root.status === 'failed' ? 1 : 0,
    pending: root.status === 'pending' ? 1 : 0,
  };
  
  if (root.children) {
    for (const child of root.children) {
      const childStats = getTreeStats(child);
      stats.total += childStats.total;
      stats.running += childStats.running;
      stats.completed += childStats.completed;
      stats.failed += childStats.failed;
      stats.pending += childStats.pending;
    }
  }
  
  return stats;
}

/**
 * Get all node IDs in a tree
 */
export function getAllNodeIds(root: AgentNode | null): Set<string> {
  const ids = new Set<string>();
  
  function walk(node: AgentNode) {
    ids.add(node.id);
    if (node.children) {
      for (const child of node.children) {
        walk(child);
      }
    }
  }
  
  if (root) walk(root);
  return ids;
}
