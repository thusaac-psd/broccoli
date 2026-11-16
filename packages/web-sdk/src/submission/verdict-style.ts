import {
  AlertCircle,
  CheckCircle2,
  Clock,
  type LucideIcon,
  MinusCircle,
  XCircle,
} from 'lucide-react';

import type { ShownStatus, Verdict } from './types.ts';
import { getVerdictKey, type VerdictKey } from './verdict-key.ts';

/**
 * How a test case's verdict looks wherever one is shown: its icon, and the
 * Tailwind classes for its text, its surface (resting and hovered or
 * selected) and its edge (resting and selected).
 */
export interface VerdictStyle {
  icon: LucideIcon;
  text: string;
  surface: string;
  surfaceActive: string;
  edge: string;
  edgeActive: string;
}

function tone(
  icon: LucideIcon,
  text: string,
  surface: string,
  surfaceActive: string,
  edge: string,
  edgeActive: string,
): VerdictStyle {
  return { icon, text, surface, surfaceActive, edge, edgeActive };
}

const VERDICT_STYLES: Record<VerdictKey, VerdictStyle> = {
  accepted: tone(
    CheckCircle2,
    'text-green-600 dark:text-green-400',
    'bg-green-500/10',
    'bg-green-500/20',
    'border-green-500/30',
    'border-green-500/60',
  ),
  wrong_answer: tone(
    XCircle,
    'text-red-600 dark:text-red-400',
    'bg-red-500/10',
    'bg-red-500/20',
    'border-red-500/30',
    'border-red-500/60',
  ),
  time_limit: tone(
    Clock,
    'text-amber-600 dark:text-amber-400',
    'bg-amber-500/10',
    'bg-amber-500/20',
    'border-amber-500/30',
    'border-amber-500/60',
  ),
  memory_limit: tone(
    Clock,
    'text-orange-600 dark:text-orange-400',
    'bg-orange-500/10',
    'bg-orange-500/20',
    'border-orange-500/30',
    'border-orange-500/60',
  ),
  runtime_error: tone(
    AlertCircle,
    'text-purple-600 dark:text-purple-400',
    'bg-purple-500/10',
    'bg-purple-500/20',
    'border-purple-500/30',
    'border-purple-500/60',
  ),
  system_error: tone(
    AlertCircle,
    'text-gray-600 dark:text-gray-400',
    'bg-gray-500/10',
    'bg-gray-500/20',
    'border-gray-500/30',
    'border-gray-500/60',
  ),
  skipped: tone(
    MinusCircle,
    'text-gray-500 dark:text-gray-400',
    'bg-gray-400/10',
    'bg-gray-400/20',
    'border-gray-400/30',
    'border-gray-400/60',
  ),
  cancelled: tone(
    MinusCircle,
    'text-gray-500 dark:text-gray-400',
    'bg-gray-400/10',
    'bg-gray-400/20',
    'border-gray-400/30',
    'border-gray-400/60',
  ),
  custom: tone(
    AlertCircle,
    'text-blue-600 dark:text-blue-400',
    'bg-blue-500/10',
    'bg-blue-500/20',
    'border-blue-500/30',
    'border-blue-500/60',
  ),
  pending: tone(
    Clock,
    'text-gray-500 dark:text-gray-400',
    'bg-gray-500/10',
    'bg-gray-500/20',
    'border-gray-500/30',
    'border-gray-500/60',
  ),
};

/**
 * The style of a test case's `verdict` under its submission's or
 * judgement's `status`, which tells a withheld verdict from one not judged
 * yet (`getVerdictKey`).
 */
export function verdictStyle(
  verdict: Verdict | null | undefined,
  status: ShownStatus,
): VerdictStyle {
  return VERDICT_STYLES[getVerdictKey(verdict, status)];
}
