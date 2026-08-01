'use client';

import { memo, useEffect, useRef, useState } from 'react';
import { LG_MEDIA_QUERY } from '@/lib/responsive-layout';
import { useMediaQuery } from '@/hooks/use-media-query';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { cn } from '@/lib/utils';
import { getCurrentMission, type Mission } from '@/lib/api';
import { BrainLogo } from '@/components/icons';
import {
  ChevronDown,
} from 'lucide-react';
import {
  Archive,
  ChatCircleText,
  ChatsCircle,
  Kanban,
  CheckCircle,
  CirclesFour,
  CircleNotch,
  Cpu,
  GearSix,
  GitBranch,
  GithubLogo,
  Key,
  Layout,
  Lightning,
  Lock,
  MagnifyingGlass,
  Plugs,
  Pulse,
  Robot,
  SidebarSimple,
  Sparkle,
  TerminalWindow,
  Toolbox,
  TreeStructure,
  XCircle,
  type Icon,
} from '@phosphor-icons/react';

type NavItem = {
  name: string;
  href: string;
  icon: Icon;
  children?: { name: string; href: string; icon: Icon }[];
};

const navigation: NavItem[] = [
  { name: 'Projets', href: '/', icon: Kanban },
  { name: 'Overview', href: '/overview', icon: Layout },
  { name: 'Mission', href: '/control', icon: ChatCircleText },
  { name: 'Workspaces', href: '/workspaces', icon: SidebarSimple },
  { name: 'Console', href: '/console', icon: TerminalWindow },
  {
    name: 'Assistant',
    href: '/assistant',
    icon: Robot,
    children: [{ name: 'Chat Hermes', href: '/assistant/chat', icon: ChatsCircle }],
  },
  { name: 'Routing', href: '/model-routing', icon: GitBranch },
  {
    name: 'Library',
    href: '/config',
    icon: TreeStructure,
    children: [
      { name: 'Commands', href: '/config/commands', icon: TerminalWindow },
      { name: 'Skills', href: '/config/skills', icon: Lightning },
      { name: 'Workspaces', href: '/config/workspace-templates', icon: CirclesFour },
      { name: 'Profiles', href: '/config/settings', icon: GearSix },
    ],
  },
  {
    name: 'Inspect',
    href: '/inspect',
    icon: MagnifyingGlass,
    children: [
      { name: 'System', href: '/inspect/system', icon: Pulse },
      { name: 'MCPs', href: '/inspect/mcps', icon: Plugs },
      { name: 'Tools', href: '/inspect/tools', icon: Toolbox },
    ],
  },
  {
    name: 'Settings',
    href: '/settings',
    icon: GearSix,
    children: [
      { name: 'Backends', href: '/settings/backends', icon: Cpu },
      { name: 'Providers', href: '/settings/providers', icon: Key },
      { name: 'GitHub', href: '/settings/github', icon: GithubLogo },
      { name: 'LLM', href: '/settings/llm', icon: Sparkle },
      { name: 'Security', href: '/settings/secrets', icon: Lock },
      { name: 'Data', href: '/settings/data', icon: Archive },
    ],
  },
];

export const Sidebar = memo(function Sidebar({
  id,
  open = false,
  onClose,
}: {
  id?: string;
  open?: boolean;
  onClose?: () => void;
}) {
  const pathname = usePathname();
  const asideRef = useRef<HTMLElement>(null);
  const isDesktop = useMediaQuery(LG_MEDIA_QUERY);
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

  // Trap focus inside the off-canvas drawer while it is open on mobile.
  useEffect(() => {
    if (!open || isDesktop) return;
    const root = asideRef.current;
    if (!root) return;

    const focusables = root.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    if (focusables.length === 0) return;

    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    first.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Tab') return;
      if (event.shiftKey) {
        if (document.activeElement === first) {
          event.preventDefault();
          last.focus();
        }
        return;
      }
      if (document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    root.addEventListener('keydown', onKeyDown);
    return () => root.removeEventListener('keydown', onKeyDown);
  }, [open, isDesktop]);

  // Auto-expand sections if we're on their subpages
  useEffect(() => {
    const timer = window.setTimeout(() => {
      if (pathname.startsWith('/config')) {
        setExpandedItems((prev) => new Set([...prev, 'Library']));
      }
      if (pathname.startsWith('/inspect')) {
        setExpandedItems((prev) => new Set([...prev, 'Inspect']));
      }
      if (pathname.startsWith('/settings')) {
        setExpandedItems((prev) => new Set([...prev, 'Settings']));
      }
    }, 0);
    return () => window.clearTimeout(timer);
  }, [pathname]);

  // Fetch current mission periodically. Activity status (the bottom-left
  // "Working" / "Ready" pill) is derived from this; previously the Sidebar
  // also subscribed to the global control SSE stream to flip the pill in
  // real time, but that subscription was the single biggest source of
  // re-renders in the app: every status frame from the backend (multiple
  // times per second on a busy mission) caused the entire nav tree to
  // re-commit. The 10s poll is good enough for a non-critical indicator
  // and the cost is paid once per page rather than per-event.
  useEffect(() => {
    let cancelled = false;
    const fetchMission = async () => {
      try {
        const mission = await getCurrentMission();
        if (!cancelled) {
          setCurrentMission((prev) => {
            // Bail out when nothing changed so the memoized Sidebar doesn't
            // re-render on every successful poll for no reason.
            if (prev?.id === mission?.id && prev?.status === mission?.status) {
              return prev;
            }
            return mission;
          });
        }
      } catch {
        // ignore errors
      }
    };

    fetchMission();
    const interval = setInterval(fetchMission, 10000); // Update every 10s
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  const isActive = currentMission?.status === 'active';
  const StatusIcon = isActive
    ? CircleNotch
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
  const hideClosedMobileNav = !open && !isDesktop;

  return (
    <aside
      ref={asideRef}
      id={id}
      aria-label="Main navigation"
      aria-hidden={hideClosedMobileNav ? true : undefined}
      inert={hideClosedMobileNav ? true : undefined}
      className={cn(
        'fixed left-0 top-0 z-40 flex h-screen w-56 flex-col glass-panel border-r border-white/[0.06]',
        // Off-canvas drawer below lg; always docked at lg+ so desktop is
        // unchanged (lg:translate-x-0 wins regardless of `open`).
        'transition-transform duration-200 lg:translate-x-0',
        open ? 'translate-x-0' : '-translate-x-full',
      )}
    >
      {/* Header */}
      <div className="flex h-16 items-center gap-2 border-b border-white/[0.06] px-4">
        <BrainLogo size={32} />
        <div className="flex flex-col">
          <span className="text-sm font-medium text-white">Sandboxed.sh</span>
          <span className="tag">v{process.env.NEXT_PUBLIC_APP_VERSION}</span>
        </div>
      </div>

      {/* Navigation - scrollable when content overflows */}
      <nav className="flex-1 overflow-y-auto p-3 space-y-1">
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
                          onClick={onClose}
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
              onClick={onClose}
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
          onClick={onClose}
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
});
