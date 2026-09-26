import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQueryClient } from '@tanstack/react-query';
import {
  ArrowRight,
  Check,
  Hourglass,
  PartyPopper,
  ShieldAlert,
  Swords,
} from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { Link } from 'react-router';

import { useBracketApi } from './hooks/useBracketApi';
import { useContestProblems } from './hooks/useContestData';
import { problemPath } from './lib/links';
import { playerLabel } from './lib/player';
import { roundNameKey } from './lib/rounds';
import { matchSteps, playerStage, type Side, type Step } from './lib/stage';
import { OrderingPanel } from './OrderingPanel';
import { useGameClock, useStartsIn } from './parts';
import type { MatchView } from './types';

interface MyMatchPanelProps {
  contestId: number;
  match: MatchView;
  side: Side;
  onOpenDetails: () => void;
}

/**
 * The player's own match, pinned above the bracket: where they are in it
 * and the one thing they should do next.
 */
export function MyMatchPanel({
  contestId,
  match,
  side,
  onOpenDetails,
}: MyMatchPanelProps) {
  const { t } = useTranslation();
  const opponentName =
    side === 'a'
      ? playerLabel(match.player_b_name, match.player_b)
      : playerLabel(match.player_a_name, match.player_a);
  const myScore = side === 'a' ? match.score_a : match.score_b;
  const theirScore = side === 'a' ? match.score_b : match.score_a;

  return (
    <section className="overflow-hidden rounded-xl border border-primary/30 bg-card shadow-sm">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-border bg-primary/5 px-5 py-3">
        <Swords className="h-4 w-4 text-primary" />
        <div className="text-sm">
          <span className="font-semibold">
            {t('codelink-bracket.my.title')}
          </span>
          <span className="text-muted-foreground">
            {' · '}
            {t(roundNameKey(match.round))}
            {' · '}
            {t('codelink-bracket.my.vs', { name: opponentName })}
          </span>
        </div>
        <div className="ml-auto flex items-center gap-3">
          <span className="font-mono text-lg font-bold tabular-nums">
            {myScore} : {theirScore}
          </span>
          <Button size="sm" variant="ghost" onClick={onOpenDetails}>
            {t('codelink-bracket.my.details')}
          </Button>
        </div>
      </div>
      <div className="space-y-5 px-5 py-4">
        <Stepper steps={matchSteps(match, side)} />
        <CurrentTask contestId={contestId} match={match} side={side} />
      </div>
    </section>
  );
}

function Stepper({ steps }: { steps: Step[] }) {
  const { t } = useTranslation();
  const label = (s: Step) =>
    s.key === 'game'
      ? t('codelink-bracket.game.regular', { n: (s.game ?? 0) + 1 })
      : t(`codelink-bracket.step.${s.key}`);
  return (
    <ol className="flex items-center gap-1 overflow-x-auto">
      {steps.map((s, i) => (
        <li
          key={`${s.key}-${s.game ?? ''}`}
          className="flex items-center gap-1"
        >
          {i > 0 && (
            <span
              aria-hidden
              className={cn(
                'h-px w-4 sm:w-8',
                s.status === 'upcoming' ? 'bg-border' : 'bg-primary/50',
              )}
            />
          )}
          <span
            aria-current={s.status === 'current' ? 'step' : undefined}
            className={cn(
              'flex items-center gap-1.5 whitespace-nowrap rounded-full px-2.5 py-1 text-xs font-medium',
              s.status === 'done' && 'text-primary',
              s.status === 'current' &&
                'bg-primary text-primary-foreground shadow-sm',
              s.status === 'upcoming' && 'text-muted-foreground',
            )}
          >
            {s.status === 'done' && <Check className="h-3 w-3" />}
            {label(s)}
          </span>
        </li>
      ))}
    </ol>
  );
}

function opponentName(match: MatchView, side: Side): string {
  return side === 'a'
    ? playerLabel(match.player_b_name, match.player_b)
    : playerLabel(match.player_a_name, match.player_a);
}

function WaitingStart({
  startsAtMs,
  opponent,
}: {
  startsAtMs: number | null;
  opponent: string;
}) {
  const { t } = useTranslation();
  const startsIn = useStartsIn(startsAtMs);
  return (
    <div className="flex items-center gap-3 text-sm">
      <Check className="h-4 w-4 text-emerald-600" />
      <p>
        {startsIn === null
          ? t('codelink-bracket.my.waitingRanking', { name: opponent })
          : startsIn === 'now'
            ? t('codelink-bracket.card.starting')
            : t('codelink-bracket.my.startsIn', { time: startsIn })}
      </p>
    </div>
  );
}

function CurrentTask({
  contestId,
  match,
  side,
}: {
  contestId: number;
  match: MatchView;
  side: Side;
}) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const queryClient = useQueryClient();
  const { byId: problems } = useContestProblems(contestId);
  const clock = useGameClock(match);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const stage = playerStage(match, side);

  const note = (icon: ReactNode, text: string, tone = '') => (
    <div className={cn('flex items-center gap-3 text-sm', tone)}>
      {icon}
      <p>{text}</p>
    </div>
  );

  switch (stage.kind) {
    case 'waiting_opponent':
      return note(
        <Hourglass className="h-4 w-4 text-muted-foreground" />,
        t('codelink-bracket.my.waitingOpponent'),
      );
    case 'rank':
      return (
        <div className="space-y-2">
          <OrderingPanel
            contestId={contestId}
            opponentGroup={side === 'a' ? match.group_b : match.group_a}
            submittedOrder={null}
            problems={problems}
            submitting={submitting}
            onSubmit={async (order) => {
              setError(null);
              setSubmitting(true);
              try {
                await api.submitOrder(contestId, match.id, order);
                await queryClient.invalidateQueries({
                  queryKey: ['codelink-bracket-bracket', contestId],
                });
              } catch (err) {
                setError(
                  err instanceof Error
                    ? err.message
                    : t('codelink-bracket.error.submitOrder'),
                );
              } finally {
                setSubmitting(false);
              }
            }}
          />
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
      );
    case 'waiting_start':
      return (
        <WaitingStart
          startsAtMs={stage.startsAtMs}
          opponent={opponentName(match, side)}
        />
      );
    case 'play': {
      const p =
        stage.problemId === null ? undefined : problems.get(stage.problemId);
      const gameName =
        stage.game >= 3
          ? t('codelink-bracket.game.tiebreak', { n: stage.game - 2 })
          : t('codelink-bracket.game.regular', { n: stage.game + 1 });
      return (
        <div className="flex flex-wrap items-center gap-4 rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-4">
          <div className="min-w-0 flex-1">
            <div className="text-xs font-medium uppercase tracking-wide text-emerald-700 dark:text-emerald-300">
              {gameName}
            </div>
            <div className="mt-0.5 truncate text-base font-semibold">
              {p
                ? `${p.label} · ${p.title}`
                : t('codelink-bracket.my.problemHidden')}
            </div>
          </div>
          {clock && (
            <div className="text-right">
              <div className="text-xs text-muted-foreground">
                {t('codelink-bracket.my.timeLeft')}
              </div>
              <div className="font-mono text-2xl font-bold tabular-nums">
                {clock.text}
              </div>
            </div>
          )}
          {stage.problemId !== null && (
            <Button asChild>
              <Link to={problemPath(contestId, stage.problemId)}>
                {t('codelink-bracket.my.solve')}
                <ArrowRight className="ml-1.5 h-4 w-4" />
              </Link>
            </Button>
          )}
        </div>
      );
    }
    case 'judging':
      return note(
        <Hourglass className="h-4 w-4 text-amber-600" />,
        t('codelink-bracket.my.judging'),
      );
    case 'staff':
      return note(
        <ShieldAlert className="h-4 w-4 text-red-600" />,
        t('codelink-bracket.my.staff'),
      );
    case 'advanced':
      return note(
        <PartyPopper className="h-4 w-4 text-amber-500" />,
        match.round >= 4
          ? t('codelink-bracket.my.champion')
          : t('codelink-bracket.my.advanced', {
              round: t(roundNameKey(match.round + 1)),
            }),
        'font-medium',
      );
    case 'eliminated':
      return note(
        <span className="h-4 w-4" />,
        t('codelink-bracket.my.eliminated'),
        'text-muted-foreground',
      );
  }
}
