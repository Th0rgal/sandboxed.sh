/**
 * Agent Tree Types
 * 
 * Type definitions for the dynamic agent tree visualization.
 */

export type AgentType = 
  | 'OpenCode' 
  | 'OpenCodeSession';

export type AgentStatus = 
  | 'running' 
  | 'completed' 
  | 'failed' 
  | 'pending' 
  | 'paused' 
  | 'cancelled';

export interface AgentNode {
  id: string;
  type: AgentType;
  status: AgentStatus;
  name: string;
  description: string;
  model?: string;
  budgetAllocated: number;
  budgetSpent: number;
  complexity?: number;
  children?: AgentNode[];
  /** Depth in tree (computed) */
  depth?: number;
  /** Position for rendering (computed) */
  x?: number;
  y?: number;
}

export interface TreeLayout {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  width: number;
  height: number;
}

export interface LayoutNode {
  id: string;
  x: number;
  y: number;
  agent: AgentNode;
}

export interface LayoutEdge {
  id: string;
  from: { x: number; y: number };
  to: { x: number; y: number };
  status: AgentStatus;
}

export interface TreeStats {
  total: number;
  running: number;
  completed: number;
  failed: number;
  pending: number;
}
