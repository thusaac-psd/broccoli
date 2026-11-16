import type { PrintStatus } from '../types';

// Mirrors the server-side page estimate (plugins/print/src/models.rs).
const WRAP_COLS = 90;
const LINES_PER_PAGE = 54;

export function estimatePages(source: string): number {
  if (!source) return 1;
  let visual = 0;
  for (const line of source.split('\n')) {
    const w = [...line].length;
    visual += w === 0 ? 1 : Math.ceil(w / WRAP_COLS);
  }
  return Math.max(1, Math.ceil(Math.max(1, visual) / LINES_PER_PAGE));
}

/** Compact relative time from an epoch-seconds value (e.g. "2m ago"). */
export function formatRelative(epochSecs: number | null | undefined): string {
  if (!epochSecs) return '—';
  const deltaSec = Math.max(0, Date.now() / 1000 - epochSecs);
  if (deltaSec < 5) return 'just now';
  if (deltaSec < 60) return `${Math.floor(deltaSec)}s ago`;
  const min = Math.floor(deltaSec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ${min % 60}m ago`;
  const day = Math.floor(hr / 24);
  return `${day}d ago`;
}

/**
 * Visual metadata per status. Each status gets a semantic colour that
 * reads well in both light and dark modes. The status stylesheet imports
 * `@broccoli/web-sdk/plugin.css`, which registers the default Tailwind
 * palette so utilities like `bg-amber-500/12` are emitted.
 */
export interface StatusMeta {
  labelKey: string;
  /** Tailwind classes for the pill (bg + text + border). */
  pill: string;
  /** Tailwind class for the leading dot. */
  dot: string;
  pulse: boolean;
}

export const STATUS_META: Record<PrintStatus, StatusMeta> = {
  pending_approval: {
    labelKey: 'print.status.pending_approval',
    pill: 'bg-amber-500/12 text-amber-600 dark:text-amber-400 border-amber-500/30',
    dot: 'bg-amber-500',
    pulse: true,
  },
  pending: {
    labelKey: 'print.status.pending',
    pill: 'bg-blue-500/12 text-blue-600 dark:text-blue-400 border-blue-500/30',
    dot: 'bg-blue-500',
    pulse: false,
  },
  claimed: {
    labelKey: 'print.status.claimed',
    pill: 'bg-sky-500/12 text-sky-600 dark:text-sky-400 border-sky-500/30',
    dot: 'bg-sky-500',
    pulse: true,
  },
  printing: {
    labelKey: 'print.status.printing',
    pill: 'bg-violet-500/12 text-violet-600 dark:text-violet-400 border-violet-500/30',
    dot: 'bg-violet-500',
    pulse: true,
  },
  done: {
    labelKey: 'print.status.done',
    pill: 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-400 border-emerald-500/30',
    dot: 'bg-emerald-500',
    pulse: false,
  },
  failed: {
    labelKey: 'print.status.failed',
    pill: 'bg-red-500/12 text-red-600 dark:text-red-400 border-red-500/30',
    dot: 'bg-red-500',
    pulse: false,
  },
  canceled: {
    labelKey: 'print.status.canceled',
    pill: 'bg-muted text-muted-foreground border-border',
    dot: 'bg-muted-foreground/50',
    pulse: false,
  },
};

export const ALL_STATUSES: PrintStatus[] = [
  'pending_approval',
  'pending',
  'claimed',
  'printing',
  'done',
  'failed',
  'canceled',
];
