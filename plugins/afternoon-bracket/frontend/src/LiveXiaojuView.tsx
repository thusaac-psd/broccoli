import { useAuth } from '@broccoli/web-sdk/auth';
import type { Submission } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';

import { useBracketApi } from './hooks/useBracketApi';
import {
  deriveCountdownStatus,
  formatCountdownLabel,
  xiaojuTimingFromMatch,
} from './lib/countdown';
import { describeMatchPhase, isActivelyPlaying } from './lib/phase';
import type { MatchView } from './types';

/**
 * Structurally matches (a subset of) the host's
 * `packages/web/src/features/submission/hooks/use-submissions.ts`
 * `SubmissionEntry` -- that type is host-internal and not exported by
 * `@broccoli/web-sdk`, so this plugin declares its own local shape rather
 * than importing across that boundary. `Submission` itself IS a web-sdk
 * type, so `submission` below is exact, not approximated.
 */
interface LiveSubmissionEntry {
  id: number;
  submission: Submission | null;
  status: 'submitting' | 'polling' | 'done' | 'error';
}

interface LiveXiaojuViewProps {
  contestId?: number;
  contestType?: string;
  problemId?: number;
  submissions?: LiveSubmissionEntry[];
}

function matchTouchesProblem(match: MatchView, problemId: number): boolean {
  const fields: (readonly (number | null)[] | number | null)[] = [
    match.group_a,
    match.group_b,
    match.order_a ?? [null, null, null],
    match.order_b ?? [null, null, null],
    match.tiebreak_problem,
  ];
  return fields.some((field) =>
    Array.isArray(field) ? field.includes(problemId) : field === problemId,
  );
}

/**
 * Live 小局 view: registered at `problem-detail.sidebar`
 * (contest_type = "afternoon-bracket", position = "prepend"). Shows this
 * viewer's match status for the problem they are currently looking at, plus
 * their own submissions for it (via the `submissions` slot prop the host
 * already scopes to this problem -- no separate fetch needed for that part).
 *
 * Renders nothing when the current problem is not tied to any match this
 * viewer participates in (e.g. a spectator with no match of their own, or a
 * problem outside this bracket) -- this is additive sidebar content, not a
 * required element of the page.
 */
export function LiveXiaojuView({
  contestId,
  problemId,
  submissions,
}: LiveXiaojuViewProps) {
  const api = useBracketApi();
  const auth = useAuth();
  const viewerId = auth.user?.id ?? null;

  const { data } = useQuery({
    queryKey: ['afternoon-bracket-bracket', contestId],
    enabled: !!contestId && viewerId !== null,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval: 5_000,
  });

  if (!contestId || !problemId || viewerId === null || !data) {
    return null;
  }

  const match = data.matches.find(
    (m) =>
      (m.player_a === viewerId || m.player_b === viewerId) &&
      matchTouchesProblem(m, problemId),
  );
  if (!match) {
    return null;
  }

  const status = describeMatchPhase(match.state);
  const countdown = deriveCountdownStatus(
    xiaojuTimingFromMatch(match),
    Date.now(),
  );
  const ownScore = viewerId === match.player_a ? match.score_a : match.score_b;
  const opponentScore =
    viewerId === match.player_a ? match.score_b : match.score_a;

  return (
    <div className="mb-4 rounded-md border border-border p-3">
      <div className="mb-1 flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          下午场 Bracket -- {status.label}
        </h4>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {ownScore} - {opponentScore}
        </span>
      </div>

      {match.state === 'awaiting_judge' && (
        <p className="text-xs text-amber-700">
          This 小局's deadline passed while an earlier submission was still
          being judged -- you are not simply still playing. Waiting on the
          judge.
        </p>
      )}

      {(isActivelyPlaying(match.state) || match.state === 'awaiting_judge') && (
        <p
          className={cn(
            'text-xs',
            countdown.kind === 'waiting-for-server'
              ? 'text-amber-700'
              : 'text-muted-foreground',
          )}
        >
          {formatCountdownLabel(countdown)}
        </p>
      )}

      {submissions && submissions.length > 0 && (
        <ul className="mt-2 flex flex-col gap-1">
          {submissions.slice(0, 5).map((entry) => (
            <li key={entry.id} className="text-xs text-muted-foreground">
              {entry.submission
                ? `${entry.submission.status}${
                    entry.submission.result?.verdict
                      ? ` -- ${entry.submission.result.verdict}`
                      : ''
                  }`
                : entry.status}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
