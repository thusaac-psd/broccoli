// When the contest problem list must be fetched again without the viewer
// doing anything. What a viewer may see changes over a running contest
// (the start, or a plugin revealing problems as a round begins), and the
// host has no push channel for it, so the list polls while the contest runs.

import type { ContestPhase } from '@broccoli/web-sdk/contest';

/** How often a running contest's problem list is re-fetched. */
export const CONTEST_PROBLEMS_REFRESH_MS = 60_000;

/**
 * TanStack Query `refetchInterval` for the problem list: poll while the
 * server reports the contest running, not before it starts or after it ends.
 */
export function problemListRefetchInterval(
  contest: { phase: ContestPhase } | undefined,
): number | false {
  return contest?.phase === 'running' ? CONTEST_PROBLEMS_REFRESH_MS : false;
}
