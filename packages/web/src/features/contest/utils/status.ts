import { serverNow } from '@broccoli/web-sdk/api';
import type { BadgeProps } from '@broccoli/web-sdk/ui';

export type ContestPhase = 'upcoming' | 'running' | 'ended';

/**
 * Where `now` falls in a contest window. The end instant still counts as
 * running, as on the server.
 */
export function contestPhase(
  startMs: number,
  endMs: number,
  now: number,
): ContestPhase {
  return now < startMs ? 'upcoming' : now <= endMs ? 'running' : 'ended';
}

/**
 * A contest's state as an admin sees it: its phase once activated, or why
 * contestants cannot see it. Without `activate_time` the contest is a draft
 * only `contest:manage` sees; before it, it is scheduled; from
 * `deactivate_time` on it is hidden again.
 */
export type ContestStatus =
  | 'draft'
  | 'scheduled'
  | ContestPhase
  | 'deactivated';

/** The timestamps of a contest that decide its status. */
export interface ContestWindow {
  activate_time?: string | null;
  start_time: string;
  end_time: string;
  deactivate_time?: string | null;
}

export function contestStatus(
  window: ContestWindow,
  now: number,
): ContestStatus {
  if (!window.activate_time) return 'draft';
  if (now < Date.parse(window.activate_time)) return 'scheduled';
  if (window.deactivate_time && now >= Date.parse(window.deactivate_time)) {
    return 'deactivated';
  }
  return contestPhase(
    Date.parse(window.start_time),
    Date.parse(window.end_time),
    now,
  );
}

const STATUS_BADGES: Record<
  ContestStatus,
  { key: string; variant: NonNullable<BadgeProps['variant']> }
> = {
  draft: { key: 'contests.draft', variant: 'outline' },
  scheduled: { key: 'contests.scheduled', variant: 'outline' },
  upcoming: { key: 'contests.upcoming', variant: 'outline' },
  running: { key: 'contests.running', variant: 'default' },
  ended: { key: 'contests.ended', variant: 'secondary' },
  deactivated: { key: 'contests.deactivated', variant: 'secondary' },
};

export function getContestStatus(
  window: ContestWindow,
  t: (key: string) => string,
): { label: string; variant: NonNullable<BadgeProps['variant']> } {
  const { key, variant } = STATUS_BADGES[contestStatus(window, serverNow())];
  return { label: t(key), variant };
}
