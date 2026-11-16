import type { ContestPhase } from '@broccoli/web-sdk/contest';
import { PLUGIN_OUTAGE_POLL_MS } from '@broccoli/web-sdk/hooks';
import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import type { PluginOutage } from '@broccoli/web-sdk/submission';
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

/** Whether the contest has ended: ended, or deactivated after. */
export function phaseHasEnded(phase: ContestPhase): boolean {
  return phase === 'ended' || phase === 'deactivated';
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

/**
 * How often a contest whose phase change is due is asked for it, until the
 * server reports the new phase (its clock may be a moment behind ours).
 */
export const PHASE_DUE_POLL_MS = 1_000;

/**
 * Seats spread their first ask after a phase change over this long, so a
 * contest hall does not ask at the same server instant.
 */
export const PHASE_DUE_SPREAD_MS = 3_000;

/** The longest wait a browser timer holds (longer ones fire at once). */
const MAX_TIMER_MS = 2 ** 31 - 1;

/**
 * The pace of the contest query (its `refetchInterval`, through
 * `pollInterval`): the wait until its phase is due to change, plus this
 * seat's `offsetMs`; once due, `PHASE_DUE_POLL_MS`; while its plugin is out,
 * at most `PLUGIN_OUTAGE_POLL_MS`; with neither, `false`. `nowMs` is the
 * server's clock (`serverNow()`). Before the change the answer moves with the
 * clock but always names the same instant, and after it the answer stays
 * put, so a timer restarted on every render still fires.
 */
export function contestPollPace(
  contest: {
    phase: ContestPhase;
    start_time: string;
    end_time: string;
    plugin_outage?: PluginOutage | null;
  },
  nowMs: number,
  offsetMs: number,
): number | false {
  const due = nextPhaseChange(contest);
  const untilDue =
    due === null
      ? Infinity
      : Math.max(due + offsetMs - nowMs, 0) || PHASE_DUE_POLL_MS;
  const pace = Math.min(
    untilDue,
    contest.plugin_outage ? PLUGIN_OUTAGE_POLL_MS : Infinity,
  );
  return pace === Infinity ? false : Math.min(pace, MAX_TIMER_MS);
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
