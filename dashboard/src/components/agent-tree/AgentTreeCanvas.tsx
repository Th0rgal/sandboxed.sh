'use client';

/**
 * AgentTreeCanvas
 * 
 * Dynamic, animated visualization of the agent execution tree.
 * Uses framer-motion for smooth animations and SVG for rendering.
 */

import { useEffect, useMemo, useRef, useState, useCallback } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { cn } from '@/lib/utils';
import { formatCents } from '@/lib/utils';
import type { AgentNode, LayoutNode, LayoutEdge } from './types';
import { computeLayout, getTreeStats } from './layout';
import {
  Bot,
  GitBranch,
  CheckCircle,
  XCircle,
  Loader,
  Clock,
  Ban,
  ChevronLeft,
  X,
} from 'lucide-react';

// Node dimensions
const NODE_WIDTH = 140;
const NODE_HEIGHT = 80;

// Status colors
const STATUS_COLORS = {
  running: { 
    bg: 'rgba(99, 102, 241, 0.15)', 
    border: 'rgba(99, 102, 241, 0.6)',
    glow: 'rgba(99, 102, 241, 0.4)',
    text: '#818cf8',
    line: '#6366f1',
  },
  completed: { 
    bg: 'rgba(16, 185, 129, 0.15)', 
    border: 'rgba(16, 185, 129, 0.5)',
    glow: 'rgba(16, 185, 129, 0.2)',
    text: '#34d399',
    line: '#10b981',
  },
  failed: { 
    bg: 'rgba(239, 68, 68, 0.15)', 
    border: 'rgba(239, 68, 68, 0.5)',
    glow: 'rgba(239, 68, 68, 0.2)',
    text: '#f87171',
    line: '#ef4444',
  },
  pending: { 
    bg: 'rgba(251, 191, 36, 0.1)', 
    border: 'rgba(251, 191, 36, 0.3)',
    glow: 'rgba(251, 191, 36, 0.1)',
    text: '#fbbf24',
    line: 'rgba(251, 191, 36, 0.5)',
  },
  paused: { 
    bg: 'rgba(255, 255, 255, 0.03)', 
    border: 'rgba(255, 255, 255, 0.1)',
    glow: 'transparent',
    text: 'rgba(255, 255, 255, 0.4)',
    line: 'rgba(255, 255, 255, 0.2)',
  },
  cancelled: { 
    bg: 'rgba(255, 255, 255, 0.03)', 
    border: 'rgba(255, 255, 255, 0.1)',
    glow: 'transparent',
    text: 'rgba(255, 255, 255, 0.4)',
    line: 'rgba(255, 255, 255, 0.2)',
  },
};

// Agent type icons
const AGENT_ICONS = {
  OpenCode: Bot,
  OpenCodeSession: GitBranch,
};

interface AgentTreeCanvasProps {
  tree: AgentNode | null;
  onSelectNode?: (node: AgentNode | null) => void;
  selectedNodeId?: string | null;
  className?: string;
  /** Compact mode for embedded panels - hides minimap and details panel */
  compact?: boolean;
}

/**
 * Animated edge component with gradient and glow
 */
function AnimatedEdge({ edge, index }: { edge: LayoutEdge; index: number }) {
  const colors = STATUS_COLORS[edge.status];
  const pathId = `edge-path-${edge.id}`;
  
  // Curved path from parent to child
  const midY = (edge.from.y + edge.to.y) / 2;
  const path = `M ${edge.from.x} ${edge.from.y} 
                C ${edge.from.x} ${midY}, 
                  ${edge.to.x} ${midY}, 
                  ${edge.to.x} ${edge.to.y}`;

  return (
    <motion.g
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.3, delay: index * 0.02 }}
    >
      {/* Glow effect */}
      <motion.path
        d={path}
        fill="none"
        stroke={colors.glow}
        strokeWidth={8}
        strokeLinecap="round"
        initial={{ pathLength: 0 }}
        animate={{ pathLength: 1 }}
        transition={{ duration: 0.5, delay: index * 0.05 }}
        style={{ filter: 'blur(4px)' }}
      />
      
      {/* Main line */}
      <motion.path
        d={path}
        fill="none"
        stroke={colors.line}
        strokeWidth={2}
        strokeLinecap="round"
        initial={{ pathLength: 0 }}
        animate={{ pathLength: 1 }}
        transition={{ duration: 0.5, delay: index * 0.05 }}
      />
      
      {/* Animated pulse for running edges */}
      {edge.status === 'running' && (
        <motion.circle
          r={4}
          fill={colors.text}
          initial={{ opacity: 0 }}
          animate={{ opacity: [0, 1, 0] }}
          transition={{ duration: 1.5, repeat: Infinity }}
        >
          <animateMotion
            dur="1.5s"
            repeatCount="indefinite"
            path={path}
          />
        </motion.circle>
      )}
    </motion.g>
  );
}

/**
 * Animated node component
 */
function AnimatedNode({ 
  layoutNode, 
  isSelected,
  onClick,
  index,
}: { 
  layoutNode: LayoutNode;
  isSelected: boolean;
  onClick: () => void;
  index: number;
}) {
  const { agent, x, y } = layoutNode;
  const colors = STATUS_COLORS[agent.status];
  const Icon = AGENT_ICONS[agent.type] ?? Bot;
  
  // Format model name (remove provider prefix)
  const displayModel = agent.model 
    ? agent.model.includes('/') 
      ? agent.model.split('/').pop() 
      : agent.model
    : null;

  // Truncate model name for display
  const shortModel = displayModel 
    ? displayModel.length > 14 
      ? displayModel.slice(0, 12) + '…' 
      : displayModel
    : null;

  return (
    <motion.g
      initial={{ opacity: 0, scale: 0.8 }}
      animate={{ 
        opacity: 1, 
        scale: 1,
        transition: { 
          type: 'spring', 
          stiffness: 300, 
          damping: 25,
          delay: index * 0.03,
        }
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      style={{ cursor: 'pointer' }}
      onClick={onClick}
    >
      {/* Glow effect for running nodes */}
      {agent.status === 'running' && (
        <motion.rect
          x={x - 5}
          y={y - 5}
          width={NODE_WIDTH + 10}
          height={NODE_HEIGHT + 10}
          rx={16}
          fill="none"
          stroke={colors.glow}
          strokeWidth={2}
          animate={{ 
            opacity: [0.3, 0.7, 0.3],
            scale: [1, 1.02, 1],
          }}
          transition={{ duration: 2, repeat: Infinity }}
          style={{ filter: 'blur(8px)' }}
        />
      )}
      
      {/* Selection ring */}
      {isSelected && (
        <motion.rect
          x={x - 4}
          y={y - 4}
          width={NODE_WIDTH + 8}
          height={NODE_HEIGHT + 8}
          rx={16}
          fill="none"
          stroke="#6366f1"
          strokeWidth={2}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
        />
      )}
      
      {/* Node background */}
      <rect
        x={x}
        y={y}
        width={NODE_WIDTH}
        height={NODE_HEIGHT}
        rx={12}
        fill={colors.bg}
        stroke={colors.border}
        strokeWidth={1.5}
      />
      
      {/* Icon container */}
      <rect
        x={x + 8}
        y={y + 8}
        width={28}
        height={28}
        rx={6}
        fill={colors.bg}
      />
      
      {/* Icon (using foreignObject for Lucide icons) */}
      <foreignObject x={x + 8} y={y + 8} width={28} height={28}>
        <div className="flex items-center justify-center w-full h-full">
          <Icon 
            className={cn(
              'w-4 h-4',
              agent.status === 'running' && 'animate-pulse'
            )} 
            style={{ color: colors.text }}
          />
        </div>
      </foreignObject>
      
      {/* Status icon */}
      <foreignObject x={x + NODE_WIDTH - 24} y={y + 8} width={16} height={16}>
        <div className="flex items-center justify-center w-full h-full">
          {agent.status === 'running' && (
            <Loader className="w-3 h-3 animate-spin" style={{ color: colors.text }} />
          )}
          {agent.status === 'completed' && (
            <CheckCircle className="w-3 h-3" style={{ color: colors.text }} />
          )}
          {agent.status === 'failed' && (
            <XCircle className="w-3 h-3" style={{ color: colors.text }} />
          )}
          {agent.status === 'pending' && (
            <Clock className="w-3 h-3" style={{ color: colors.text }} />
          )}
          {agent.status === 'cancelled' && (
            <Ban className="w-3 h-3" style={{ color: colors.text }} />
          )}
        </div>
      </foreignObject>
      
      {/* Agent name */}
      <text
        x={x + 42}
        y={y + 24}
        fill="white"
        fontSize={11}
        fontWeight={500}
      >
        {agent.name.length > 12 ? agent.name.slice(0, 10) + '…' : agent.name}
      </text>
      
      {/* Model badge */}
      {shortModel && (
        <g>
          <rect
            x={x + 8}
            y={y + 40}
            width={Math.min(shortModel.length * 5.5 + 10, NODE_WIDTH - 16)}
            height={16}
            rx={4}
            fill="rgba(255, 255, 255, 0.06)"
          />
          <text
            x={x + 13}
            y={y + 52}
            fill="rgba(255, 255, 255, 0.6)"
            fontSize={9}
            fontFamily="monospace"
          >
            {shortModel}
          </text>
        </g>
      )}
      
      {/* Cost display */}
      <text
        x={x + NODE_WIDTH - 8}
        y={y + NODE_HEIGHT - 10}
        fill={colors.text}
        fontSize={10}
        fontWeight={500}
        textAnchor="end"
        fontFamily="monospace"
      >
        {formatCents(agent.budgetSpent)}
      </text>
      
      {/* Budget total */}
      <text
        x={x + NODE_WIDTH - 8}
        y={y + NODE_HEIGHT - 10}
        fill="rgba(255, 255, 255, 0.3)"
        fontSize={9}
        textAnchor="end"
        fontFamily="monospace"
        dx={-35}
      >
        / {formatCents(agent.budgetAllocated)}
      </text>
    </motion.g>
  );
}

/**
 * Node details panel (slide-in)
 */
function NodeDetailsPanel({ 
  node, 
  onClose 
}: { 
  node: AgentNode; 
  onClose: () => void;
}) {
  const colors = STATUS_COLORS[node.status];
  const Icon = AGENT_ICONS[node.type];
  
  return (
    <motion.div
      initial={{ x: 320, opacity: 0 }}
      animate={{ x: 0, opacity: 1 }}
      exit={{ x: 320, opacity: 0 }}
      className="absolute right-0 top-0 h-full w-80 glass-panel border-l border-white/[0.06] z-20 flex flex-col"
    >
      {/* Header */}
      <div className="flex items-start justify-between border-b border-white/[0.06] p-4">
        <div className="flex items-center gap-3">
          <button
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <ChevronLeft className="h-5 w-5" />
          </button>
          <div 
            className="flex h-10 w-10 items-center justify-center rounded-xl"
            style={{ backgroundColor: colors.bg }}
          >
            <Icon className="h-5 w-5" style={{ color: colors.text }} />
          </div>
          <div>
            <h2 className="text-lg font-medium text-white">{node.name}</h2>
            <p className="text-xs capitalize" style={{ color: colors.text }}>
              {node.status}
            </p>
          </div>
        </div>
        <button
          onClick={onClose}
          className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
        >
          <X className="h-5 w-5" />
        </button>
      </div>
      
      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4 space-y-5">
        <div>
          <label className="text-[10px] uppercase tracking-wider text-white/40">Type</label>
          <p className="text-sm text-white mt-1">{node.type}</p>
        </div>
        
        <div>
          <label className="text-[10px] uppercase tracking-wider text-white/40">Description</label>
          <p className="text-sm text-white/80 mt-1">{node.description}</p>
        </div>
        
        {node.model && (
          <div>
            <label className="text-[10px] uppercase tracking-wider text-white/40">Model</label>
            <div className="mt-1 px-2 py-1 rounded-md bg-white/[0.04] inline-block">
              <span className="text-sm font-mono text-white">
                {node.model.includes('/') ? node.model.split('/').pop() : node.model}
              </span>
            </div>
          </div>
        )}
        
        <div>
          <label className="text-[10px] uppercase tracking-wider text-white/40">Budget</label>
          <div className="mt-2">
            <div className="flex justify-between text-sm">
              <span className="text-white tabular-nums">
                {formatCents(node.budgetSpent)}
              </span>
              <span className="text-white/40">
                of {formatCents(node.budgetAllocated)}
              </span>
            </div>
            <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-white/[0.08]">
              <motion.div
                className="h-full rounded-full"
                style={{ backgroundColor: colors.line }}
                initial={{ width: 0 }}
                animate={{ 
                  width: `${Math.min(100, (node.budgetSpent / node.budgetAllocated) * 100)}%` 
                }}
                transition={{ duration: 0.5 }}
              />
            </div>
          </div>
        </div>
        
        {node.complexity !== undefined && (
          <div>
            <label className="text-[10px] uppercase tracking-wider text-white/40">Complexity</label>
            <div className="mt-2 flex items-center gap-2">
              <div className="flex-1 h-1.5 overflow-hidden rounded-full bg-white/[0.08]">
                <motion.div
                  className="h-full rounded-full bg-amber-500"
                  initial={{ width: 0 }}
                  animate={{ width: `${node.complexity * 100}%` }}
                  transition={{ duration: 0.5 }}
                />
              </div>
              <span className="text-sm text-white tabular-nums">
                {(node.complexity * 100).toFixed(0)}%
              </span>
            </div>
          </div>
        )}
      </div>
    </motion.div>
  );
}

/**
 * Mini-map showing tree statistics
 */
function TreeMiniMap({ tree }: { tree: AgentNode | null }) {
  const stats = useMemo(() => getTreeStats(tree), [tree]);
  
  if (!tree) return null;
  
  return (
    <div className="absolute bottom-4 left-4 p-4 rounded-xl bg-black/40 backdrop-blur-sm border border-white/[0.06]">
      <div className="grid grid-cols-2 gap-3 text-xs">
        <div>
          <div className="text-white/40">Total</div>
          <div className="text-lg font-light text-white tabular-nums">{stats.total}</div>
        </div>
        <div>
          <div className="text-emerald-400/60">Done</div>
          <div className="text-lg font-light text-emerald-400 tabular-nums">{stats.completed}</div>
        </div>
        <div>
          <div className="text-indigo-400/60">Running</div>
          <div className="text-lg font-light text-indigo-400 tabular-nums">{stats.running}</div>
        </div>
        <div>
          <div className="text-red-400/60">Failed</div>
          <div className="text-lg font-light text-red-400 tabular-nums">{stats.failed}</div>
        </div>
      </div>
    </div>
  );
}

/**
 * Main tree canvas component
 */
export function AgentTreeCanvas({ 
  tree, 
  onSelectNode,
  selectedNodeId,
  className,
  compact = false,
}: AgentTreeCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [dimensions, setDimensions] = useState({ width: 800, height: 600 });
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [zoom, setZoom] = useState(1);
  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0, panX: 0, panY: 0 });
  
  // Compute layout
  const layout = useMemo(() => computeLayout(tree), [tree]);
  
  // Find selected node
  const selectedNode = useMemo(() => {
    if (!selectedNodeId) return null;
    return layout.nodes.find(n => n.id === selectedNodeId)?.agent ?? null;
  }, [layout, selectedNodeId]);
  
  // Handle container resize
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setDimensions({
          width: entry.contentRect.width,
          height: entry.contentRect.height,
        });
      }
    });
    
    observer.observe(container);
    return () => observer.disconnect();
  }, []);
  
  // Center tree on initial render or when tree changes, and auto-fit if needed
  useEffect(() => {
    if (layout.width > 0 && layout.height > 0 && dimensions.width > 0 && dimensions.height > 0) {
      // Calculate zoom to fit the tree in view with some padding
      const paddingX = compact ? 40 : 80;
      const paddingY = compact ? 40 : 80;
      const availableWidth = dimensions.width - paddingX;
      const availableHeight = dimensions.height - paddingY;
      
      const scaleX = availableWidth / layout.width;
      const scaleY = availableHeight / layout.height;
      
      // Use the smaller scale to fit both dimensions
      // Cap between 0.3/0.4 (minimum readable) and 1 (don't zoom in past 100%)
      const MIN_ZOOM = compact ? 0.3 : 0.4;
      const fitZoom = Math.max(MIN_ZOOM, Math.min(1, Math.min(scaleX, scaleY)));
      
      // Calculate pan to center horizontally, start from top with padding
      const scaledWidth = layout.width * fitZoom;
      const centerX = (dimensions.width - scaledWidth) / 2;
      
      // If tree fits vertically, center it; otherwise start from top
      const scaledHeight = layout.height * fitZoom;
      const centerY = scaledHeight < availableHeight 
        ? (dimensions.height - scaledHeight) / 2 
        : compact ? 20 : 30; // Start near top if tree is too tall
      
      setZoom(fitZoom);
      setPan({ x: centerX, y: centerY });
    }
  }, [layout.width, layout.height, dimensions.width, dimensions.height, compact]);
  
  // Pan handlers
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return; // Only left click
    setIsDragging(true);
    setDragStart({ x: e.clientX, y: e.clientY, panX: pan.x, panY: pan.y });
  }, [pan]);
  
  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    if (!isDragging) return;
    setPan({
      x: dragStart.panX + (e.clientX - dragStart.x),
      y: dragStart.panY + (e.clientY - dragStart.y),
    });
  }, [isDragging, dragStart]);
  
  const handleMouseUp = useCallback(() => {
    setIsDragging(false);
  }, []);
  
  // Zoom handler - reduced sensitivity for smoother zooming
  const handleWheel = useCallback((e: React.WheelEvent) => {
    e.preventDefault();
    const delta = e.deltaY > 0 ? 0.97 : 1.03; // Reduced from 0.9/1.1 for smoother zoom
    setZoom(z => Math.min(2, Math.max(0.3, z * delta))); // Min 0.3 to keep nodes readable
  }, []);
  
  if (!tree) {
    return (
      <div className={cn(
        'flex items-center justify-center h-full',
        className
      )}>
        <div className="text-center">
          <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-2xl bg-white/[0.02]">
            <Bot className="h-8 w-8 text-white/30" />
          </div>
          <p className="text-white/40">No agent tree to display</p>
        </div>
      </div>
    );
  }
  
  return (
    <div 
      ref={containerRef}
      className={cn(
        'relative overflow-hidden bg-gradient-to-b from-black/20 to-black/40',
        isDragging ? 'cursor-grabbing' : 'cursor-grab',
        className
      )}
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      onMouseLeave={handleMouseUp}
      onWheel={handleWheel}
    >
      <svg
        width={dimensions.width}
        height={dimensions.height}
        className="select-none"
      >
        <defs>
          {/* Gradient definitions for edges */}
          <linearGradient id="edge-gradient-running" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="rgba(99, 102, 241, 0.8)" />
            <stop offset="100%" stopColor="rgba(99, 102, 241, 0.3)" />
          </linearGradient>
          <linearGradient id="edge-gradient-completed" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="rgba(16, 185, 129, 0.8)" />
            <stop offset="100%" stopColor="rgba(16, 185, 129, 0.3)" />
          </linearGradient>
        </defs>
        
        <g transform={`translate(${pan.x}, ${pan.y}) scale(${zoom})`}>
          {/* Edges */}
          <AnimatePresence>
            {layout.edges.map((edge, i) => (
              <AnimatedEdge key={edge.id} edge={edge} index={i} />
            ))}
          </AnimatePresence>
          
          {/* Nodes */}
          <AnimatePresence>
            {layout.nodes.map((node, i) => (
              <AnimatedNode
                key={node.id}
                layoutNode={node}
                isSelected={node.id === selectedNodeId}
                onClick={() => onSelectNode?.(node.agent)}
                index={i}
              />
            ))}
          </AnimatePresence>
        </g>
      </svg>
      
      {/* Mini-map - hidden in compact mode */}
      {!compact && <TreeMiniMap tree={tree} />}
      
      {/* Zoom controls */}
      <div className={cn("absolute flex gap-1", compact ? "bottom-2 right-2" : "bottom-4 right-4 gap-2")}>
        <button
          onClick={() => setZoom(z => Math.min(2, z * 1.15))}
          className={cn(
            "rounded-lg bg-black/40 backdrop-blur-sm border border-white/[0.06] text-white/60 hover:text-white hover:bg-white/[0.04] transition-colors flex items-center justify-center",
            compact ? "w-6 h-6 text-xs" : "w-8 h-8"
          )}
        >
          +
        </button>
        <button
          onClick={() => setZoom(z => Math.max(0.3, z / 1.15))}
          className={cn(
            "rounded-lg bg-black/40 backdrop-blur-sm border border-white/[0.06] text-white/60 hover:text-white hover:bg-white/[0.04] transition-colors flex items-center justify-center",
            compact ? "w-6 h-6 text-xs" : "w-8 h-8"
          )}
        >
          −
        </button>
        <button
          onClick={() => {
            // Fit to view with minimum zoom for readability
            const paddingX = compact ? 40 : 80;
            const paddingY = compact ? 40 : 80;
            const availableWidth = dimensions.width - paddingX;
            const availableHeight = dimensions.height - paddingY;
            
            const scaleX = availableWidth / layout.width;
            const scaleY = availableHeight / layout.height;
            const MIN_ZOOM = compact ? 0.3 : 0.4;
            const fitZoom = Math.max(MIN_ZOOM, Math.min(1, Math.min(scaleX, scaleY)));
            
            const scaledWidth = layout.width * fitZoom;
            const centerX = (dimensions.width - scaledWidth) / 2;
            
            const scaledHeight = layout.height * fitZoom;
            const centerY = scaledHeight < availableHeight 
              ? (dimensions.height - scaledHeight) / 2 
              : 20;
            
            setZoom(fitZoom);
            setPan({ x: centerX, y: centerY });
          }}
          className={cn(
            "rounded-lg bg-black/40 backdrop-blur-sm border border-white/[0.06] text-white/60 hover:text-white hover:bg-white/[0.04] transition-colors text-xs",
            compact ? "px-1.5 h-6" : "px-2 h-8"
          )}
        >
          Fit
        </button>
      </div>
      
      {/* Node details panel - hidden in compact mode */}
      {!compact && (
        <AnimatePresence>
          {selectedNode && (
            <NodeDetailsPanel 
              node={selectedNode} 
              onClose={() => onSelectNode?.(null)} 
            />
          )}
        </AnimatePresence>
      )}
    </div>
  );
}

export default AgentTreeCanvas;
