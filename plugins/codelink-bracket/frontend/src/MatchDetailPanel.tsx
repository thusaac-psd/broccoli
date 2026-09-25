import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
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
import { playerLabel } from './lib/player';
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
  const { t } = useTranslation();
  const api = useBracketApi();
  const auth = useAuth();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const [submittingOrder, setSubmittingOrder] = useState(false);
  const [startingMatch, setStartingMatch] = useState(false);
  const [forceDeciding, setForceDeciding] = useState(false);

  const { data: match, isLoading } = useQuery({
    queryKey: ['codelink-bracket-match', contestId, matchId],
    queryFn: () => api.getMatch(contestId, matchId),
    refetchInterval: (query) => {
      const current = query.state.data as MatchView | undefined;
      if (!current) return 4_000;
      return current.state === 'decided' ? false : 4_000;
    },
  });

  // Contest labels for the ordering buttons. The host's list is already
  // visibility-filtered for this viewer, so it never names a hidden problem.
  const { data: problemLabels = new Map<number, string>() } = useQuery({
    queryKey: ['codelink-bracket-problem-labels', contestId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}/problems', {
        params: { path: { id: contestId } },
      });
      if (error || !data) return new Map<number, string>();
      return new Map(data.map((p) => [p.problem_id, p.label]));
    },
  });

  const invalidate = () => {
    queryClient.invalidateQueries({
      queryKey: ['codelink-bracket-match', contestId, matchId],
    });
    queryClient.invalidateQueries({
      queryKey: ['codelink-bracket-bracket', contestId],
    });
  };

  if (isLoading || !match) {
    return (
      <p className="p-3 text-sm text-muted-foreground">
        {t('codelink-bracket.match.loading')}
      </p>
    );
  }

  const viewerId = auth.user?.id ?? null;
  const permissions = auth.user?.permissions ?? [];
  const isStaff = permissions.includes(CONTEST_MANAGE);
  const canViewAll = permissions.includes(SUBMISSION_VIEW_ALL);

  const status = describeMatchPhase(match.state);
  const nameA = playerLabel(match.player_a_name, match.player_a);
  const nameB = playerLabel(match.player_b_name, match.player_b);

  // See `MatchState`'s doc comment (plugins/codelink-bracket/src/model.rs):
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
        err instanceof Error
          ? err.message
          : t('codelink-bracket.error.submitOrder'),
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
        err instanceof Error ? err.message : t('codelink-bracket.error.start'),
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
        err instanceof Error
          ? err.message
          : t('codelink-bracket.error.forceDecide'),
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
          {t('codelink-bracket.match.heading', {
            round: match.round,
            a: nameA,
            b: nameB,
          })}
        </h3>
        <StatusBadge kind={status.kind} label={t(status.labelKey)} />
      </div>

      <div className="text-sm text-muted-foreground">
        {t('codelink-bracket.match.score', {
          a: match.score_a,
          b: match.score_b,
        })}
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
          {formatCountdownLabel(countdown, t)}
        </div>
      )}

      {match.state === 'awaiting_judge' && (
        <div className="rounded-md border border-amber-500/30 bg-amber-500/10 p-3 text-sm text-amber-700">
          {t('codelink-bracket.match.awaitingJudge')}
          {match.awaiting_submission_id !== null && (
            <>
              {' '}
              {t('codelink-bracket.match.blockingSubmission', {
                id: match.awaiting_submission_id,
              })}
            </>
          )}
        </div>
      )}

      {match.state === 'needs_adjudication' && (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-sm text-red-700">
          {/* Escalation from `awaiting_judge` keeps the blocking id; the
              tiebreak list running out never sets one. */}
          {match.awaiting_submission_id !== null
            ? t('codelink-bracket.match.stuckJudge', {
                id: match.awaiting_submission_id,
              })
            : t('codelink-bracket.match.tiebreakExhausted')}
        </div>
      )}

      {match.state === 'ordering' && viewerRole !== null && (
        <OrderingPanel
          opponentGroup={opponentGroup}
          onSubmit={handleSubmitOrder}
          submitting={submittingOrder}
          alreadySubmitted={ownSubmittedOrder !== null}
          problemLabels={problemLabels}
        />
      )}

      {canViewAll && (
        <SpectatorFeed
          contestId={contestId}
          playerA={match.player_a}
          playerB={match.player_b}
          nameA={nameA}
          nameB={nameB}
        />
      )}

      {isStaff && (
        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {t('codelink-bracket.staff.title')}
          </h4>
          <div className="flex flex-wrap gap-2">
            {match.state === 'ordering' && (
              <Button
                type="button"
                size="sm"
                disabled={!bothOrdersIn || startingMatch}
                onClick={handleStart}
              >
                {startingMatch
                  ? t('codelink-bracket.staff.starting')
                  : t('codelink-bracket.staff.start')}
              </Button>
            )}
            {(isActivelyPlaying(match.state) ||
              match.state === 'awaiting_judge' ||
              match.state === 'needs_adjudication') && (
              <>
                {/* Expiry re-runs the normal decision, which never reopens
                    an escalated match -- only an explicit award does. */}
                {match.state !== 'needs_adjudication' && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={forceDeciding}
                    onClick={() => handleForceDecide(null)}
                  >
                    {t('codelink-bracket.staff.forceExpiry')}
                  </Button>
                )}
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={forceDeciding}
                  onClick={() => handleForceDecide(match.player_a)}
                >
                  {t('codelink-bracket.staff.awardTo', { name: nameA })}
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={forceDeciding}
                  onClick={() => handleForceDecide(match.player_b)}
                >
                  {t('codelink-bracket.staff.awardTo', { name: nameB })}
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
