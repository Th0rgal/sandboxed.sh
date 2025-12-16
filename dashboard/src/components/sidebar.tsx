'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { cn } from '@/lib/utils';
import {
  LayoutDashboard,
  MessageSquare,
  Network,
  History,
  Terminal,
  Settings,
  Plug,
} from 'lucide-react';

const navigation = [
  { name: 'Overview', href: '/', icon: LayoutDashboard },
  { name: 'Control', href: '/control', icon: MessageSquare },
  { name: 'Agents', href: '/agents', icon: Network },
  { name: 'Modules', href: '/modules', icon: Plug },
  { name: 'Console', href: '/console', icon: Terminal },
  { name: 'History', href: '/history', icon: History },
  { name: 'Settings', href: '/settings', icon: Settings },
];

export function Sidebar() {
  const pathname = usePathname();

  return (
    <aside className="fixed left-0 top-0 z-40 flex h-screen w-56 flex-col glass-panel border-r border-white/[0.06]">
      {/* Header */}
      <div className="flex h-16 items-center gap-2 border-b border-white/[0.06] px-4">
        <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-indigo-500/10">
          <span className="text-sm font-bold text-indigo-400">O</span>
        </div>
        <div className="flex flex-col">
          <span className="text-sm font-medium text-white">OpenAgent</span>
          <span className="tag">v0.1.0</span>
        </div>
      </div>

      {/* Navigation */}
      <nav className="flex flex-1 flex-col gap-1 p-3">
        {navigation.map((item) => {
          const isActive = pathname === item.href;
          return (
            <Link
              key={item.name}
              href={item.href}
              className={cn(
                'flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-all',
                isActive
                  ? 'bg-white/[0.08] text-white'
                  : 'text-white/50 hover:bg-white/[0.04] hover:text-white/80'
              )}
            >
              <item.icon className="h-[18px] w-[18px]" />
              {item.name}
            </Link>
          );
        })}
      </nav>

      {/* Footer */}
      <div className="border-t border-white/[0.06] p-4">
        <div className="flex items-center gap-3">
          <div className="flex h-8 w-8 items-center justify-center rounded-full bg-white/[0.04]">
            <span className="text-xs font-medium text-white/60">AI</span>
          </div>
          <div className="flex-1 min-w-0">
            <p className="truncate text-xs font-medium text-white/80">Agent Status</p>
            <p className="flex items-center gap-1.5 text-[10px] text-white/40">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
              Ready
            </p>
          </div>
        </div>
      </div>
    </aside>
  );
}
