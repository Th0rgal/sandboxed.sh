'use client';

import { cn } from '@/lib/utils';
import { LucideIcon } from 'lucide-react';

interface StatsCardProps {
  title: string;
  value: string | number;
  subtitle?: string;
  icon?: LucideIcon;
  trend?: {
    value: number;
    isPositive: boolean;
  };
  className?: string;
  color?: 'default' | 'success' | 'warning' | 'error' | 'info' | 'accent';
}

const labelColors = {
  default: 'text-white/40',
  success: 'text-emerald-400',
  warning: 'text-amber-400',
  error: 'text-red-400',
  info: 'text-blue-400',
  accent: 'text-indigo-400',
};

const valueColors = {
  default: 'text-white',
  success: 'text-emerald-400',
  warning: 'text-amber-400',
  error: 'text-red-400',
  info: 'text-blue-400',
  accent: 'text-indigo-400',
};

export function StatsCard({
  title,
  value,
  subtitle,
  icon: Icon,
  trend,
  className,
  color = 'default',
}: StatsCardProps) {
  return (
    <div
      className={cn(
        'stat-panel',
        className
      )}
    >
      <div className="flex items-start justify-between">
        <div className="flex-1">
          <p className={cn('stat-label', labelColors[color])}>
            {title}
          </p>
          <div className="mt-1 flex items-baseline gap-1">
            <p className={cn('stat-value', valueColors[color])}>{value}</p>
            {subtitle && (
              <span className="stat-suffix">{subtitle}</span>
            )}
          </div>
          {trend && (
            <p
              className={cn(
                'mt-1 text-xs',
                trend.isPositive ? 'text-emerald-400' : 'text-red-400'
              )}
            >
              {trend.isPositive ? '↑' : '↓'} {Math.abs(trend.value)}%
            </p>
          )}
        </div>
        {Icon && (
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-white/[0.04]">
            <Icon className="h-5 w-5 text-white/40" />
          </div>
        )}
      </div>
    </div>
  );
}
