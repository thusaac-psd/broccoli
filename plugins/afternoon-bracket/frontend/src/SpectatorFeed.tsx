import { useApiClient } from '@broccoli/web-sdk/api';
import type { SubmissionSummary } from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

interface SpectatorFeedProps {
  contestId: number;
  playerA: number;
  playerB: number;
}

/**
 * `submission:view_all` holders see both players' submission feeds live,
 * side by side. Uses the HOST's typed `GET /contests/{id}/submissions`
 * endpoint (not a plugin route -- this plugin's own backend exposes no
 * submission-listing endpoint of its own), scoped per player via `user_id`.
 */
export function SpectatorFeed({
  contestId,
  playerA,
  playerB,
}: SpectatorFeedProps) {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
      <PlayerFeed contestId={contestId} userId={playerA} label="Player A" />
      <PlayerFeed contestId={contestId} userId={playerB} label="Player B" />
    </div>
  );
}

function PlayerFeed({
  contestId,
  userId,
  label,
}: {
  contestId: number;
  userId: number;
  label: string;
}) {
  const apiClient = useApiClient();

  const { data, isLoading, isError } = useQuery({
    queryKey: ['afternoon-bracket-spectator-feed', contestId, userId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/submissions',
        {
          params: {
            path: { id: contestId },
            query: {
              user_id: userId,
              per_page: 10,
              sort_by: 'created_at',
              sort_order: 'desc',
            },
          },
        },
      );
      if (error || !data) {
        throw new Error('Failed to load submissions');
      }
      return data;
    },
    refetchInterval: 5_000,
  });

  return (
    <div className="rounded-md border border-border p-3">
      <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        {label} (user {userId})
      </h4>
      {isLoading && <p className="text-sm text-muted-foreground">Loading...</p>}
      {isError && (
        <p className="text-sm text-destructive">Failed to load submissions.</p>
      )}
      {data && data.data.length === 0 && (
        <p className="text-sm text-muted-foreground">No submissions yet.</p>
      )}
      {data && data.data.length > 0 && (
        <ul className="flex flex-col gap-1.5">
          {data.data.map((submission: SubmissionSummary) => (
            <SubmissionRow key={submission.id} submission={submission} />
          ))}
        </ul>
      )}
    </div>
  );
}

function SubmissionRow({ submission }: { submission: SubmissionSummary }) {
  const verdict = submission.verdict ?? submission.status;
  return (
    <li className="flex items-center justify-between text-sm">
      <span className="truncate">{submission.problem_title}</span>
      <span className="font-mono tabular-nums text-xs text-muted-foreground">
        {verdict}
        {submission.score !== null && submission.score !== undefined
          ? ` (${submission.score})`
          : ''}
      </span>
    </li>
  );
}
