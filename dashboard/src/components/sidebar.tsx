'use client';

import { useEffect, useState } from 'react';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { cn } from '@/lib/utils';
import { getCurrentMission, streamControl, type Mission, type ControlRunState } from '@/lib/api';
import { BrainLogo } from '@/components/icons';
import {
  LayoutDashboard,
  MessageSquare,
  Terminal,
  Settings,
  Loader,
  CheckCircle,
  XCircle,
  ChevronDown,
  Plug,
  FileCode,
  Server,
  Puzzle,
  ScrollText,
  Wrench,
  LayoutGrid,
  Library,
  Cog,
} from 'lucide-react';

type NavItem = {
  name: string;
  href: string;
  icon: React.ComponentType<{ className?: string }>;
  children?: { name: string; href: string; icon: React.ComponentType<{ className?: string }> }[];
};

const navigation: NavItem[] = [
  { name: 'Overview', href: '/', icon: LayoutDashboard },
  { name: 'Mission', href: '/control', icon: MessageSquare },
  { name: 'Workspaces', href: '/workspaces', icon: Server },
  { name: 'Console', href: '/console', icon: Terminal },
  {
    name: 'Library',
    href: '/config',
    icon: Library,
    children: [
      { name: 'Commands', href: '/config/commands', icon: Terminal },
      { name: 'Skills', href: '/config/skills', icon: FileCode },
      { name: 'Rules', href: '/config/rules', icon: ScrollText },
      { name: 'Workspaces', href: '/config/workspace-templates', icon: LayoutGrid },
      { name: 'Configs', href: '/config/settings', icon: Cog },
    ],
  },
  {
    name: 'Extensions',
    href: '/extensions',
    icon: Puzzle,
    children: [
      { name: 'MCP Servers', href: '/extensions/mcps', icon: Plug },
      { name: 'Plugins', href: '/extensions/plugins', icon: Puzzle },
      { name: 'Tools', href: '/extensions/tools', icon: Wrench },
    ],
  },
  { name: 'Settings', href: '/settings', icon: Settings },
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

export function Sidebar() {
  const pathname = usePathname();
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [controlState, setControlState] = useState<ControlRunState>('idle');
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

  // Auto-expand sections if we're on their subpages
  useEffect(() => {
    if (pathname.startsWith('/config')) {
      setExpandedItems((prev) => new Set([...prev, 'Library']));
    }
    if (pathname.startsWith('/extensions')) {
      setExpandedItems((prev) => new Set([...prev, 'Extensions']));
    }
  }, [pathname]);

  // Stream control events for real-time status
  useEffect(() => {
    const cleanup = streamControl((event) => {
      const data: unknown = event.data;
      if (event.type === 'status' && isRecord(data)) {
        const st = data['state'];
        setControlState(typeof st === 'string' ? (st as ControlRunState) : 'idle');
      }
    });
    
    return () => cleanup();
  }, []);

  // Fetch current mission periodically
  useEffect(() => {
    const fetchMission = async () => {
      try {
        const mission = await getCurrentMission();
        setCurrentMission(mission);
      } catch {
        // ignore errors
      }
    };

    fetchMission();
    const interval = setInterval(fetchMission, 10000); // Update every 10s
    return () => clearInterval(interval);
  }, []);

  const isActive = controlState !== 'idle';
  const StatusIcon = isActive 
    ? Loader 
    : currentMission?.status === 'completed' 
      ? CheckCircle 
      : currentMission?.status === 'failed' 
        ? XCircle 
        : null;
  const statusColor = isActive 
    ? 'text-indigo-400' 
    : currentMission?.status === 'completed' 
      ? 'text-emerald-400' 
      : currentMission?.status === 'failed' 
        ? 'text-red-400' 
        : 'text-white/40';

  return (
    <aside className="fixed left-0 top-0 z-40 flex h-screen w-56 flex-col glass-panel border-r border-white/[0.06]">
      {/* Header */}
      <div className="flex h-16 items-center gap-2 border-b border-white/[0.06] px-4">
        <BrainLogo size={32} />
        <div className="flex flex-col">
          <span className="text-sm font-medium text-white">OpenAgent</span>
          <span className="tag">v0.1.0</span>
        </div>
      </div>

      {/* Navigation */}
      <nav className="flex flex-1 flex-col gap-1 p-3">
        {navigation.map((item) => {
          const isCurrentPath = pathname === item.href;
          const isChildActive = item.children?.some((child) => pathname === child.href);
          const isExpanded = expandedItems.has(item.name);
          const showMissionIndicator = item.href === '/control' && currentMission;

          const toggleExpanded = () => {
            setExpandedItems((prev) => {
              const next = new Set(prev);
              if (next.has(item.name)) {
                next.delete(item.name);
              } else {
                next.add(item.name);
              }
              return next;
            });
          };

          // Items with children render as expandable sections
          if (item.children) {
            return (
              <div key={item.name}>
                <button
                  onClick={toggleExpanded}
                  className={cn(
                    'flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-all relative',
                    isCurrentPath || isChildActive
                      ? 'bg-white/[0.08] text-white'
                      : 'text-white/50 hover:bg-white/[0.04] hover:text-white/80'
                  )}
                >
                  <item.icon className="h-[18px] w-[18px]" />
                  {item.name}
                  <ChevronDown
                    className={cn(
                      'ml-auto h-4 w-4 transition-transform duration-200',
                      isExpanded && 'rotate-180'
                    )}
                  />
                </button>
                {isExpanded && (
                  <div className="ml-3 mt-1 space-y-1 border-l border-white/[0.06] pl-3">
                    {item.children.map((child) => {
                      const isChildCurrent = pathname === child.href;
                      return (
                        <Link
                          key={child.name}
                          href={child.href}
                          className={cn(
                            'flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-all',
                            isChildCurrent
                              ? 'bg-white/[0.08] text-white'
                              : 'text-white/50 hover:bg-white/[0.04] hover:text-white/80'
                          )}
                        >
                          <child.icon className="h-4 w-4" />
                          {child.name}
                        </Link>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          }

          // Regular items render as links
          return (
            <Link
              key={item.name}
              href={item.href}
              className={cn(
                'flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-all relative',
                isCurrentPath
                  ? 'bg-white/[0.08] text-white'
                  : 'text-white/50 hover:bg-white/[0.04] hover:text-white/80'
              )}
            >
              <item.icon className="h-[18px] w-[18px]" />
              {item.name}

              {/* Active mission indicator on Control link */}
              {showMissionIndicator && isActive && (
                <span className="absolute right-2 h-2 w-2 rounded-full bg-indigo-400 animate-pulse" />
              )}
            </Link>
          );
        })}
      </nav>

      {/* Current Mission Status */}
      {currentMission && (
        <Link 
          href={`/control?mission=${currentMission.id}`}
          className="mx-3 mb-3 p-3 rounded-xl bg-white/[0.02] border border-white/[0.04] hover:bg-white/[0.04] transition-colors"
        >
          <div className="flex items-center gap-2 mb-1.5">
            {StatusIcon && (
              <StatusIcon className={cn('h-3 w-3', statusColor, isActive && 'animate-spin')} />
            )}
            <span className="text-[10px] uppercase tracking-wider text-white/40">
              {isActive ? 'Running' : currentMission.status}
            </span>
          </div>
          <p className="text-xs text-white/70 truncate">
            {currentMission.title?.slice(0, 30) || 'Current Mission'}
          </p>
        </Link>
      )}

      {/* Footer */}
      <div className="border-t border-white/[0.06] p-4">
        <div className="flex items-center gap-3">
          <BrainLogo size={32} />
          <div className="flex-1 min-w-0">
            <p className="truncate text-xs font-medium text-white/80">Agent Status</p>
            <p className="flex items-center gap-1.5 text-[10px] text-white/40">
              <span className={cn(
                'h-1.5 w-1.5 rounded-full',
                isActive ? 'bg-indigo-400 animate-pulse' : 'bg-emerald-400'
              )} />
              {isActive ? 'Working' : 'Ready'}
            </p>
          </div>
        </div>
      </div>
    </aside>
  );
}
