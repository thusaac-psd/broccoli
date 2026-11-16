import type { PluginOutage, ShownStatus, Withheld } from '@/submission/types';

import { PLUGIN_OUTAGE_POLL_MS } from '../hooks/poll.ts';
import { isTerminalStatus } from './utils.ts';

/** What of a submission's response decides where it stands. */
export interface SubmissionOutcome {
  status?: ShownStatus;
  withheld?: Withheld | null;
  /** Set while the submission waits for its contest type's provider. */
  plugin_outage?: PluginOutage | null;
}

/**
 * Where a submission stands, as one viewer sees it. Everything that shows a
 * submission's progress (its badge, its score, its result card, a spinner)
 * and everything that polls it reads this, so they never disagree.
 *
 * - `judging`: queued or being judged now.
 * - `waiting`: not decided, and parked until its contest's plugin is back,
 *   whether its status shows or the outage withholds it. Temporary: it
 *   moves again once an administrator acts or the plugin answers.
 * - `hidden`: the contest hides the outcome from this viewer; asking again
 *   shows it hidden again until the server releases it.
 * - `final`: the outcome is decided.
 */
export type SubmissionPhase = 'judging' | 'waiting' | 'hidden' | 'final';

export function submissionPhase({
  status,
  withheld,
  plugin_outage,
}: SubmissionOutcome): SubmissionPhase {
  if (status == null) {
    return withheld?.reason === 'plugin_outage' ? 'waiting' : 'hidden';
  }
  if (isTerminalStatus(status)) return 'final';
  return plugin_outage ? 'waiting' : 'judging';
}

/** How often a submission being judged is asked for. */
export const JUDGING_POLL_MS = 1_000;

/**
 * The pace every poller of a submission or code run follows: `false` once
 * nothing more can change for this viewer.
 */
export function submissionPollPace(outcome: SubmissionOutcome): number | false {
  const phase = submissionPhase(outcome);
  switch (phase) {
    case 'judging':
      return JUDGING_POLL_MS;
    case 'waiting':
      return PLUGIN_OUTAGE_POLL_MS;
    case 'hidden':
    case 'final':
      return false;
    default:
      return phase satisfies never;
  }
}
