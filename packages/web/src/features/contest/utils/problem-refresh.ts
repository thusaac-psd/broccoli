// When the contest problem list must be fetched again without the viewer
// doing anything. What a viewer may see changes over a running contest
// (the start, or a plugin revealing problems as a round begins), and the
// host has no push channel for it, so the list polls while the contest runs.

/** How often a running contest's problem list is re-fetched. */
export const CONTEST_PROBLEMS_REFRESH_MS = 60_000;

export interface ContestWindow {
  start_time: string;
  end_time: string;
}

/**
 * TanStack Query `refetchInterval` for the problem list: poll while the
 * contest is running, not before it starts or after it ends.
 */
export function problemListRefetchInterval(
  contest: ContestWindow | undefined,
  now: number,
): number | false {
  if (!contest) return false;
  const start = Date.parse(contest.start_time);
  const end = Date.parse(contest.end_time);
  return now >= start && now < end ? CONTEST_PROBLEMS_REFRESH_MS : false;
}
