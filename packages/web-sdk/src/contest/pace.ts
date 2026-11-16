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

/**
 * How often a running contest with nothing due is asked for, so an outage
 * that starts or ends while its page is open shows within about a minute.
 * Its budget: one request per open seat per minute, so a hall of 1,000
 * seats asks about 17 times a second, spread across the minute.
 */
export const RUNNING_CONTEST_POLL_MS = 60_000;

/** When this seat asks about a contest, drawn once per page load. */
export interface SeatPace {
  /** Its place in the spread of asks after a phase change. */
  phaseOffsetMs: number;
  /**
   * Its pace for a running contest: within a quarter of
   * `RUNNING_CONTEST_POLL_MS` either way, so seats that opened the page
   * together drift apart instead of asking in lockstep.
   */
  runningPollMs: number;
}

/** A seat's pace, drawn with `random`. */
export function seatPace(random: () => number): SeatPace {
  return {
    phaseOffsetMs: Math.round(random() * PHASE_DUE_SPREAD_MS),
    runningPollMs: Math.round(RUNNING_CONTEST_POLL_MS * (0.75 + random() / 2)),
  };
}

/** This seat's pace. */
export const SEAT_PACE: SeatPace = seatPace(Math.random);

/**
 * The pace of any query that follows a contest's phase or outage, as its
 * `refetchInterval` through `pollInterval`: the contest itself, or a
 * plugin's view of it such as a scoreboard. It is the wait until the phase is
 * due to change, plus the seat's `phaseOffsetMs`; once due,
 * `PHASE_DUE_POLL_MS`; while the contest runs, at most the seat's
 * `runningPollMs`; while its plugin is out, at most `PLUGIN_OUTAGE_POLL_MS`;
 * otherwise `false`. `nowMs` is the server's clock (`serverNow()`). Before
 * the change the answer moves with the clock but always names the same
 * instant, and otherwise it stays put, so a timer restarted on every render
 * still fires.
 */
export function contestPollPace(
  contest: {
    phase: ContestPhase;
    start_time: string;
    end_time: string;
    plugin_outage?: PluginOutage | null;
  },
  nowMs: number,
  seat: SeatPace = SEAT_PACE,
): number | false {
  const due = nextPhaseChange(contest);
  const untilDue =
    due === null
      ? Infinity
      : Math.max(due + seat.phaseOffsetMs - nowMs, 0) || PHASE_DUE_POLL_MS;
  const pace = Math.min(
    untilDue,
    contest.phase === 'running' ? seat.runningPollMs : Infinity,
    contest.plugin_outage ? PLUGIN_OUTAGE_POLL_MS : Infinity,
  );
  return pace === Infinity ? false : Math.min(pace, MAX_TIMER_MS);
}
