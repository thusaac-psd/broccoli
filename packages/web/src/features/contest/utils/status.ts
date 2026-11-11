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

const STATUS_BADGES: Record<
  ContestPhase,
  { key: string; variant: NonNullable<BadgeProps['variant']> }
> = {
  upcoming: { key: 'contests.upcoming', variant: 'outline' },
  running: { key: 'contests.running', variant: 'default' },
  ended: { key: 'contests.ended', variant: 'secondary' },
};

export function getContestStatus(
  startTime: string,
  endTime: string,
  t: (key: string) => string,
): { label: string; variant: NonNullable<BadgeProps['variant']> } {
  const phase = contestPhase(
    Date.parse(startTime),
    Date.parse(endTime),
    serverNow(),
  );
  const { key, variant } = STATUS_BADGES[phase];
  return { label: t(key), variant };
}
