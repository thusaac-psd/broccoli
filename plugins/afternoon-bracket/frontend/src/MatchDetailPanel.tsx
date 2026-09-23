import { useAuth } from '@broccoli/web-sdk/auth';
import {
  CONTEST_MANAGE,
  SUBMISSION_VIEW_ALL,
} from '@broccoli/web-sdk/permissions';
import { Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';

import { useBracketApi } from './hooks/useBracketApi';
import {
  deriveCountdownStatus,
  formatCountdownLabel,
  xiaojuTimingFromMatch,
} from './lib/countdown';
import { describeMatchPhase, isActivelyPlaying } from './lib/phase';
import { OrderingPanel } from './OrderingPanel';
import { SpectatorFeed } from './SpectatorFeed';
import type { MatchView } from './types';

interface MatchDetailPanelProps {
  contestId: number;
  matchId: number;
}

export function MatchDetailPanel({
  contestId,
  matchId,
}: MatchDetailPanelProps) {
  const api = useBracketApi();
  const auth = useAuth();
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const [submittingOrder, setSubmittingOrder] = useState(false);
  const [startingMatch, setStartingMatch] = useState(false);
  const [forceDeciding, setForceDeciding] = useState(false);

  const { data: match, isLoading } = useQuery({
    queryKey: ['afternoon-bracket-match', contestId, matchId],
    queryFn: () => api.getMatch(contestId, matchId),
    refetchInterval: (query) => {
      const current = query.state.data as MatchView | undefined;
      if (!current) return 4_000;
      return current.state === 'decided' ? false : 4_000;
    },
  });

  const invalidate = () => {
    queryClient.invalidateQueries({
      queryKey: ['afternoon-bracket-match', contestId, matchId],
    });
    queryClient.invalidateQueries({
      queryKey: ['afternoon-bracket-bracket', contestId],
    });
  };

  if (isLoading || !match) {
    return (
      <p className="p-3 text-sm text-muted-foreground">Loading match...</p>
    );
  }

  const viewerId = auth.user?.id ?? null;
  const permissions = auth.user?.permissions ?? [];
  const isStaff = permissions.includes(CONTEST_MANAGE);
  const canViewAll = permissions.includes(SUBMISSION_VIEW_ALL);

  const status = describeMatchPhase(match.state);

  // See `MatchState`'s doc comment (plugins/afternoon-bracket/src/model.rs):
  // a player ranks the OPPONENT's problems, so player A's opponent group is
  // `group_b` and the result of A's ranking lands in `order_b` -- and
  // symmetrically for player B. Getting this backwards is the single most
  // likely bug in this plugin per that same doc comment.
  const viewerRole: 'player_a' | 'player_b' | null =
    viewerId !== null && viewerId === match.player_a
      ? 'player_a'
      : viewerId !== null && viewerId === match.player_b
        ? 'player_b'
        : null;

  const opponentGroup =
    viewerRole === 'player_a' ? match.group_b : match.group_a;
  const ownSubmittedOrder =
    viewerRole === 'player_a' ? match.order_b : match.order_a;

  const handleSubmitOrder = async (order: [number, number, number]) => {
    setActionError(null);
    setSubmittingOrder(true);
    try {
      await api.submitOrder(contestId, matchId, order);
      invalidate();
    } catch (err) {
      setActionError(
        err instanceof Error ? err.message : 'Failed to submit order',
      );
    } finally {
      setSubmittingOrder(false);
    }
  };

  const handleStart = async () => {
    setActionError(null);
    setStartingMatch(true);
    try {
      await api.startMatch(contestId, matchId);
      invalidate();
    } catch (err) {
      setActionError(
        err instanceof Error ? err.message : 'Failed to start match',
      );
    } finally {
      setStartingMatch(false);
    }
  };

  const handleForceDecide = async (winner: number | null) => {
    setActionError(null);
    setForceDeciding(true);
    try {
      await api.forceDecide(contestId, matchId, winner);
      invalidate();
    } catch (err) {
      setActionError(
        err instanceof Error ? err.message : 'Failed to force-decide',
      );
    } finally {
      setForceDeciding(false);
    }
  };

  const bothOrdersIn = match.order_a !== null && match.order_b !== null;
  const countdown = deriveCountdownStatus(
    xiaojuTimingFromMatch(match),
    Date.now(),
  );
  const showCountdown =
    isActivelyPlaying(match.state) || match.state === 'awaiting_judge';

  return (
    <div className="flex flex-col gap-4 rounded-lg border border-border p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">
          Round {match.round} -- Player {match.player_a} vs Player{' '}
          {match.player_b}
        </h3>
        <StatusBadge kind={status.kind} label={status.label} />
      </div>

      <div className="text-sm text-muted-foreground">
        Score: {match.score_a} - {match.score_b}
      </div>

      {showCountdown && (
        <div
          className={cn(
            'text-sm',
            countdown.kind === 'waiting-for-server'
              ? 'text-amber-700'
              : 'text-muted-foreground',
          )}
        >
          {formatCountdownLabel(countdown)}
        </div>
      )}

      {match.state === 'awaiting_judge' && (
        <div className="rounded-md border border-amber-500/30 bg-amber-500/10 p-3 text-sm text-amber-700">
          This 小局's deadline passed while an earlier submission was still
          being judged. The outcome is not yet known -- this is NOT the same as
          still being in progress.
          {match.awaiting_submission_id !== null && (
            <> Blocking submission: #{match.awaiting_submission_id}.</>
          )}
        </div>
      )}

      {match.state === 'needs_adjudication' && (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-sm text-red-700">
          The tiebreak problem list was exhausted without a decision. Staff must
          resolve this match manually.
        </div>
      )}

      {match.state === 'ordering' && viewerRole !== null && (
        <OrderingPanel
          opponentGroup={opponentGroup}
          onSubmit={handleSubmitOrder}
          submitting={submittingOrder}
          alreadySubmitted={ownSubmittedOrder !== null}
        />
      )}

      {canViewAll && (
        <SpectatorFeed
          contestId={contestId}
          playerA={match.player_a}
          playerB={match.player_b}
        />
      )}

      {isStaff && (
        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Staff controls
          </h4>
          <div className="flex flex-wrap gap-2">
            {match.state === 'ordering' && (
              <Button
                type="button"
                size="sm"
                disabled={!bothOrdersIn || startingMatch}
                onClick={handleStart}
              >
                {startingMatch ? 'Starting...' : 'Start match'}
              </Button>
            )}
            {(isActivelyPlaying(match.state) ||
              match.state === 'awaiting_judge' ||
              match.state === 'needs_adjudication') && (
              <>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={forceDeciding}
                  onClick={() => handleForceDecide(null)}
                >
                  Force expiry
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={forceDeciding}
                  onClick={() => handleForceDecide(match.player_a)}
                >
                  Award to Player {match.player_a}
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={forceDeciding}
                  onClick={() => handleForceDecide(match.player_b)}
                >
                  Award to Player {match.player_b}
                </Button>
              </>
            )}
          </div>
        </div>
      )}

      {actionError && <p className="text-sm text-destructive">{actionError}</p>}
    </div>
  );
}

function StatusBadge({ kind, label }: { kind: string; label: string }) {
  return (
    <span
      className={cn(
        'inline-flex items-center rounded-md px-2.5 py-0.5 text-xs font-semibold',
        kind === 'playing' && 'bg-emerald-500/10 text-emerald-600',
        kind === 'awaiting_judge' && 'bg-amber-500/10 text-amber-700',
        kind === 'needs_adjudication' && 'bg-red-500/10 text-red-700',
        kind === 'decided' && 'bg-gray-500/10 text-gray-600',
        kind === 'ordering' && 'bg-blue-500/10 text-blue-600',
        kind === 'not_started' && 'bg-gray-500/10 text-gray-500',
      )}
    >
      {label}
    </span>
  );
}
