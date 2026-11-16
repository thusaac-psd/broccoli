import { PLUGIN_OUTAGE_POLL_MS } from '../hooks/poll.ts';
import type { PluginOutage } from '../submission/types.ts';
import type { ContestPhase } from './types.ts';

// When to ask the server about a contest again. The server decides the
// phase on its own clock; these only decide when to ask.

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

/** This seat's place in the spread of asks after a phase change. */
export const SEAT_PHASE_OFFSET_MS = Math.round(
  Math.random() * PHASE_DUE_SPREAD_MS,
);

/**
 * The pace of any query that follows a contest's phase or outage, as its
 * `refetchInterval` through `pollInterval`: the contest itself, or a
 * plugin's view of it such as a scoreboard. It is the wait until the phase is
 * due to change, plus this seat's `offsetMs`; once due, `PHASE_DUE_POLL_MS`; while its plugin is out,
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
  offsetMs: number = SEAT_PHASE_OFFSET_MS,
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
