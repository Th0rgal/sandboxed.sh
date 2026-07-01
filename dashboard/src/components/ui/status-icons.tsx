import {
  Clock,
  Bell,
  AlertTriangle,
  type LucideIcon,
} from 'lucide-react';
import {
  CheckCircle,
  CircleNotch,
  Prohibit,
  Question,
  XCircle,
  type Icon,
} from '@phosphor-icons/react';
import type { MissionStatus } from '@/lib/api';

type StatusIcon = LucideIcon | Icon;

/**
 * Unified icon mapping for mission statuses. Single source of truth — the
 * string-typed twin in `@/lib/mission-status` has been removed.
 */
export const STATUS_ICONS: Record<string, StatusIcon> = {
  pending: Clock,
  active: CircleNotch,
  running: CircleNotch,
  waiting_background: CircleNotch,
  awaiting_user: Bell,
  acknowledged: CheckCircle,
  completed: CheckCircle,
  failed: XCircle,
  cancelled: Prohibit,
  interrupted: Prohibit,
  blocked: AlertTriangle,
  not_feasible: XCircle,
  unknown: Question,
};

/**
 * Get the icon component for a mission status.
 * @param status - The mission status
 * @param fallback - Fallback icon (default: Clock)
 */
export function getStatusIcon(status: MissionStatus | string, fallback: StatusIcon = Clock): StatusIcon {
  return STATUS_ICONS[status] || fallback;
}
