'use client';

import { useEffect, useRef } from 'react';
import { X, AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description: string;
  confirmLabel?: string;
  cancelLabel?: string;
  variant?: 'danger' | 'warning' | 'default';
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = 'Confirm',
  cancelLabel = 'Cancel',
  variant = 'default',
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null);

  // Handle escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && open) {
        onCancel();
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onCancel]);

  // Handle click outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(e.target as Node)) {
        onCancel();
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [open, onCancel]);

  if (!open) return null;

  const variantStyles = {
    danger: {
      icon: 'bg-red-500/10',
      iconColor: 'text-red-400',
      button: 'bg-red-500 hover:bg-red-600',
    },
    warning: {
      icon: 'bg-amber-500/10',
      iconColor: 'text-amber-400',
      button: 'bg-amber-500 hover:bg-amber-600',
    },
    default: {
      icon: 'bg-indigo-500/10',
      iconColor: 'text-indigo-400',
      button: 'bg-indigo-500 hover:bg-indigo-600',
    },
  };

  const styles = variantStyles[variant];

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />
      
      {/* Dialog */}
      <div
        ref={dialogRef}
        className="relative w-full max-w-md rounded-2xl bg-[#1a1a1a] border border-white/[0.06] p-6 shadow-xl animate-in fade-in zoom-in-95 duration-200"
      >
        <button
          onClick={onCancel}
          className="absolute right-4 top-4 p-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
        >
          <X className="h-4 w-4" />
        </button>

        <div className="flex items-start gap-4">
          <div className={cn('flex h-10 w-10 shrink-0 items-center justify-center rounded-xl', styles.icon)}>
            <AlertTriangle className={cn('h-5 w-5', styles.iconColor)} />
          </div>
          <div className="flex-1">
            <h3 className="text-lg font-semibold text-white">{title}</h3>
            <p className="mt-2 text-sm text-white/60">{description}</p>
          </div>
        </div>

        <div className="mt-6 flex items-center justify-end gap-3">
          <button
            onClick={onCancel}
            className="px-4 py-2 rounded-lg text-sm font-medium text-white/70 hover:text-white hover:bg-white/[0.08] transition-colors"
          >
            {cancelLabel}
          </button>
          <button
            onClick={onConfirm}
            className={cn('px-4 py-2 rounded-lg text-sm font-medium text-white transition-colors', styles.button)}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
