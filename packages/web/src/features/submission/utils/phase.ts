import {
  isTerminalStatus,
  type PluginOutage,
  type ShownStatus,
} from '@broccoli/web-sdk/submission';

/**
 * Where a submission stands, as one viewer sees it, derived once from its
 * status and whether it waits for its contest's plugin. Everything that shows
 * a submission's progress (its badge, its score, its result card) reads this,
 * so they never disagree: a submission waiting for a disabled plugin is not
 * also "in progress", and nothing spins for it.
 *
 * - `withheld`: the outcome is left out for this viewer (`status` `null`).
 * - `waiting`: not final, and parked until its contest's plugin is back.
 * - `judging`: queued or being judged now.
 * - `final`: the outcome is decided.
 */
export type SubmissionPhase = 'withheld' | 'waiting' | 'judging' | 'final';

export function submissionPhase({
  status,
  plugin_outage,
}: {
  status?: ShownStatus;
  plugin_outage?: PluginOutage | null;
}): SubmissionPhase {
  if (status == null) return 'withheld';
  if (isTerminalStatus(status)) return 'final';
  return plugin_outage ? 'waiting' : 'judging';
}
