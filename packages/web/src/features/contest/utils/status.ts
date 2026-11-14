import type { ContestPhase } from '@broccoli/web-sdk/contest';
import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import type { BadgeProps } from '@broccoli/web-sdk/ui';

import type { MessageKey } from '@/lib/i18n/catalog';

/**
 * Where a contest is now, as the server reports it in every contest
 * response. The server decides it on its own clock; the web never derives it
 * from the timeline.
 */
export type { ContestPhase };

/**
 * A contest's state as an admin sees it: its phase, with an unactivated
 * contest told apart by whether it has an activate time at all (a draft only
 * `contest:manage` sees) or is waiting for it (scheduled).
 */
export type ContestStatus =
  | 'draft'
  | 'scheduled'
  | Exclude<ContestPhase, 'unactivated'>;

/** The fields of a contest response that decide its status. */
export interface ContestStatusSource {
  phase: ContestPhase;
  activate_time?: string | null;
}

export function contestStatus(contest: ContestStatusSource): ContestStatus {
  if (contest.phase !== 'unactivated') return contest.phase;
  return contest.activate_time ? 'scheduled' : 'draft';
}

/** Whether the contest has started: running, ended, or deactivated after. */
export function phaseHasStarted(phase: ContestPhase): boolean {
  return phase === 'running' || phase === 'ended' || phase === 'deactivated';
}

/**
 * The instant the contest's phase next changes for a contestant, while it is
 * upcoming or running: when to ask the server for its phase again.
 */
export function nextPhaseChange(contest: {
  phase: ContestPhase;
  start_time: string;
  end_time: string;
}): number | null {
  if (contest.phase === 'upcoming') return Date.parse(contest.start_time);
  if (contest.phase === 'running') return Date.parse(contest.end_time);
  return null;
}

const STATUS_BADGES: Record<
  ContestStatus,
  { key: MessageKey; variant: NonNullable<BadgeProps['variant']> }
> = {
  draft: { key: 'contests.draft', variant: 'outline' },
  scheduled: { key: 'contests.scheduled', variant: 'outline' },
  upcoming: { key: 'contests.upcoming', variant: 'outline' },
  running: { key: 'contests.running', variant: 'default' },
  ended: { key: 'contests.ended', variant: 'secondary' },
  deactivated: { key: 'contests.deactivated', variant: 'secondary' },
};

export function getContestStatus(
  contest: ContestStatusSource,
  t: I18nContextValue['t'],
): { label: string; variant: NonNullable<BadgeProps['variant']> } {
  const { key, variant } = STATUS_BADGES[contestStatus(contest)];
  return { label: t(key), variant };
}
