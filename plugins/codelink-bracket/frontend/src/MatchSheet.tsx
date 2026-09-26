import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  CONTEST_MANAGE,
  SUBMISSION_VIEW_ALL,
} from '@broccoli/web-sdk/permissions';
import {
  Button,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  Skeleton,
} from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  ExternalLink,
  Hourglass,
  Play,
  RotateCcw,
  Trophy,
} from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { Link } from 'react-router';

import { useBracketApi } from './hooks/useBracketApi';
import {
  type ProblemInfo,
  useContestProblems,
  usePlayerSubmissions,
} from './hooks/useContestData';
import { problemPath, submissionPath } from './lib/links';
import { playerLabel } from './lib/player';
import { roundNameKey } from './lib/rounds';
import { sideOf } from './lib/stage';
import { adjudicationMessage } from './lib/summary';
import { OrderingPanel } from './OrderingPanel';
import {
  ConfirmButton,
  StatusPill,
  useGameClock,
  useStartsIn,
  VerdictBadge,
} from './parts';
import type { GameView, MatchView } from './types';

interface MatchSheetProps {
  contestId: number;
  matchId: number | null;
  onClose: () => void;
}

export function MatchSheet({ contestId, matchId, onClose }: MatchSheetProps) {
  const { t } = useTranslation();
  return (
    <Sheet open={matchId !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent size="2xl" className="w-full overflow-y-auto p-0 sm:w-3/4">
        {/* Always present, even while the match loads: a dialog without a
            title is unnamed for screen readers (Radix warns about it). */}
        <SheetTitle className="sr-only">
          {t('codelink-bracket.my.details')}
        </SheetTitle>
        {matchId !== null && (
          <MatchDetail contestId={contestId} matchId={matchId} />
        )}
      </SheetContent>
    </Sheet>
  );
}

function MatchDetail({
  contestId,
  matchId,
}: {
  contestId: number;
  matchId: number;
}) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const apiClient = useApiClient();
  const auth = useAuth();
  const queryClient = useQueryClient();
  const { byId: problems } = useContestProblems(contestId);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const { data: match } = useQuery({
    queryKey: ['codelink-bracket-match', contestId, matchId],
    queryFn: () => api.getMatch(contestId, matchId),
    refetchInterval: (q) =>
      (q.state.data as MatchView | undefined)?.state === 'decided'
        ? false
        : 3_000,
  });

  const permissions = auth.user?.permissions ?? [];
  const isStaff = permissions.includes(CONTEST_MANAGE);
  const canViewAll = permissions.includes(SUBMISSION_VIEW_ALL);

  if (!match) {
    return (
      <div className="space-y-4 p-6">
        <Skeleton className="h-6 w-48" />
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-40 w-full" />
      </div>
    );
  }

  const nameA = playerLabel(match.player_a_name, match.player_a);
  const nameB = playerLabel(match.player_b_name, match.player_b);
  const side = sideOf(match, auth.user?.id ?? null);

  const run = async (action: () => Promise<unknown>, failKey: string) => {
    setError(null);
    setBusy(true);
    try {
      await action();
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: ['codelink-bracket-match', contestId, matchId],
        }),
        queryClient.invalidateQueries({
          queryKey: ['codelink-bracket-bracket', contestId],
        }),
      ]);
    } catch (err) {
      setError(err instanceof Error ? err.message : t(failKey));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col">
      <SheetHeader className="border-b border-border px-6 pb-4 pt-6 text-left">
        <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {t(roundNameKey(match.round))}
          <span aria-hidden>·</span>
          {t('codelink-bracket.match.number', { n: match.pos + 1 })}
        </div>
        <SheetDescription asChild>
          <div className="pt-3">
            <Scoreline match={match} nameA={nameA} nameB={nameB} />
          </div>
        </SheetDescription>
      </SheetHeader>

      <div className="space-y-6 px-6 py-5">
        <AttentionBanner
          match={match}
          contestId={contestId}
          isStaff={isStaff}
          busy={busy}
          onRejudge={(id) =>
            run(
              () => rejudgeWith(apiClient, id),
              'codelink-bracket.error.rejudge',
            )
          }
        />

        {match.state === 'ordering' && side !== null && (
          <Section title={t('codelink-bracket.section.yourRanking')}>
            <OrderingPanel
              contestId={contestId}
              opponentGroup={side === 'a' ? match.group_b : match.group_a}
              submittedOrder={side === 'a' ? match.order_b : match.order_a}
              problems={problems}
              submitting={busy}
              onSubmit={(order) =>
                run(
                  () => api.submitOrder(contestId, matchId, order),
                  'codelink-bracket.error.submitOrder',
                )
              }
            />
          </Section>
        )}

        {(match.state === 'ordering' || match.state === 'pending') && (
          <Section title={t('codelink-bracket.section.rankings')}>
            <div className="grid grid-cols-2 gap-3">
              <RankingStatus name={nameA} done={match.order_b !== null} />
              <RankingStatus name={nameB} done={match.order_a !== null} />
            </div>
            {match.state === 'ordering' && <StartNote match={match} />}
          </Section>
        )}

        {(match.games.length > 0 ||
          match.order_a !== null ||
          match.order_b !== null) && (
          <Section title={t('codelink-bracket.section.games')}>
            <GamesTable
              match={match}
              contestId={contestId}
              problems={problems}
              nameA={nameA}
              nameB={nameB}
            />
          </Section>
        )}

        {canViewAll && (
          <Section title={t('codelink-bracket.section.submissions')}>
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
              <SubmissionFeed
                contestId={contestId}
                userId={match.player_a}
                name={nameA}
                problems={problems}
              />
              <SubmissionFeed
                contestId={contestId}
                userId={match.player_b}
                name={nameB}
                problems={problems}
              />
            </div>
          </Section>
        )}

        {isStaff && (
          <StaffControls
            match={match}
            nameA={nameA}
            nameB={nameB}
            busy={busy}
            onStart={() =>
              run(
                () => api.startMatch(contestId, matchId),
                'codelink-bracket.error.start',
              )
            }
            onDecide={(winner) =>
              run(
                () => api.forceDecide(contestId, matchId, winner),
                'codelink-bracket.error.forceDecide',
              )
            }
          />
        )}

        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
      </div>
    </div>
  );
}

async function rejudgeWith(
  client: ReturnType<typeof useApiClient>,
  submissionId: number,
) {
  const { error } = await client.POST('/submissions/{id}/rejudge', {
    params: { path: { id: submissionId } },
    body: { apply_immediately: true },
  });
  if (error) throw new Error(error.message);
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-2.5">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      {children}
    </section>
  );
}

function Scoreline({
  match,
  nameA,
  nameB,
}: {
  match: MatchView;
  nameA: string;
  nameB: string;
}) {
  const clock = useGameClock(match);
  const player = (name: string, id: number, align: 'left' | 'right') => (
    <div
      className={cn(
        'flex min-w-0 flex-1 items-center gap-2',
        align === 'right' && 'flex-row-reverse text-right',
      )}
    >
      {match.winner === id && (
        <Trophy className="h-4 w-4 shrink-0 text-amber-500" />
      )}
      <span
        className={cn(
          'truncate text-lg font-semibold text-foreground',
          match.winner !== null &&
            match.winner !== id &&
            'text-muted-foreground',
        )}
      >
        {name}
      </span>
    </div>
  );
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-4">
        {player(nameA, match.player_a, 'left')}
        <div className="flex shrink-0 items-baseline gap-2 font-mono text-3xl font-bold tabular-nums text-foreground">
          <span>{match.score_a}</span>
          <span className="text-lg text-muted-foreground">:</span>
          <span>{match.score_b}</span>
        </div>
        {player(nameB, match.player_b, 'right')}
      </div>
      <div className="flex items-center justify-center gap-3">
        <StatusPill state={match.state} />
        {clock && (
          <span
            className={cn(
              'font-mono text-sm tabular-nums',
              clock.overdue
                ? 'text-amber-700 dark:text-amber-300'
                : 'text-foreground',
            )}
          >
            {clock.text}
          </span>
        )}
      </div>
    </div>
  );
}

function AttentionBanner({
  match,
  contestId,
  isStaff,
  busy,
  onRejudge,
}: {
  match: MatchView;
  contestId: number;
  isStaff: boolean;
  busy: boolean;
  onRejudge: (submissionId: number) => void;
}) {
  const { t } = useTranslation();
  const id = match.awaiting_submission_id;
  if (
    match.state !== 'awaiting_judge' &&
    match.state !== 'needs_adjudication'
  ) {
    return null;
  }
  const severe = match.state === 'needs_adjudication';
  const adjudication = adjudicationMessage(match);
  const message =
    match.state === 'awaiting_judge'
      ? t('codelink-bracket.match.awaitingJudge')
      : t(adjudication.key, adjudication.params);
  return (
    <div
      role="status"
      className={cn(
        'flex gap-3 rounded-lg border p-4 text-sm',
        severe
          ? 'border-red-500/30 bg-red-500/5 text-red-800 dark:text-red-200'
          : 'border-amber-500/30 bg-amber-500/5 text-amber-900 dark:text-amber-200',
      )}
    >
      {severe ? (
        <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      ) : (
        <Hourglass className="mt-0.5 h-4 w-4 shrink-0" />
      )}
      <div className="flex-1 space-y-3">
        <p>{message}</p>
        {id !== null && (
          <div className="flex flex-wrap gap-2">
            <Button asChild size="sm" variant="outline">
              <Link to={submissionPath(contestId, id)}>
                <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
                {t('codelink-bracket.match.openSubmission', { id })}
              </Link>
            </Button>
            {isStaff && (
              <ConfirmButton
                size="sm"
                variant="outline"
                disabled={busy}
                title={t('codelink-bracket.confirm.rejudgeTitle', { id })}
                description={t('codelink-bracket.confirm.rejudgeDescription')}
                confirmLabel={t('codelink-bracket.match.rejudge', { id })}
                onConfirm={() => onRejudge(id)}
              >
                <RotateCcw className="mr-1.5 h-3.5 w-3.5" />
                {t('codelink-bracket.match.rejudge', { id })}
              </ConfirmButton>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/** Why a ranked-or-ranking match has not started, and when it will. */
function StartNote({ match }: { match: MatchView }) {
  const { t } = useTranslation();
  const startsIn = useStartsIn(match.starts_at_ms);
  return (
    <p className="text-sm text-muted-foreground">
      {startsIn === null
        ? t('codelink-bracket.start.whenRanked')
        : startsIn === 'now'
          ? t('codelink-bracket.card.starting')
          : t('codelink-bracket.start.inTime', { time: startsIn })}
    </p>
  );
}

function RankingStatus({ name, done }: { name: string; done: boolean }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-2 rounded-lg border border-border px-3 py-2 text-sm">
      {done ? (
        <CheckCircle2 className="h-4 w-4 text-emerald-600" />
      ) : (
        <Circle className="h-4 w-4 text-muted-foreground/50" />
      )}
      <span className="truncate font-medium">{name}</span>
      <span className="ml-auto text-xs text-muted-foreground">
        {done
          ? t('codelink-bracket.ranking.done')
          : t('codelink-bracket.ranking.waiting')}
      </span>
    </div>
  );
}

function ProblemLink({
  contestId,
  problemId,
  problems,
}: {
  contestId: number;
  problemId: number | null;
  problems: ReadonlyMap<number, ProblemInfo>;
}) {
  if (problemId === null) {
    return <span className="text-muted-foreground">—</span>;
  }
  const p = problems.get(problemId);
  return (
    <Link
      to={problemPath(contestId, problemId)}
      className="font-mono font-medium text-primary hover:underline"
      title={p?.title}
    >
      {p?.label ?? `#${problemId}`}
    </Link>
  );
}

function GamesTable({
  match,
  contestId,
  problems,
  nameA,
  nameB,
}: {
  match: MatchView;
  contestId: number;
  problems: ReadonlyMap<number, ProblemInfo>;
  nameA: string;
  nameB: string;
}) {
  const { t } = useTranslation();
  const gameName = (g: GameView) =>
    g.tiebreak
      ? t('codelink-bracket.game.tiebreak', { n: g.index - 2 })
      : t('codelink-bracket.game.regular', { n: g.index + 1 });
  // Regular games that have not opened yet, from the orders -- masked for
  // players exactly like the orders themselves, so staff see what is coming
  // and players see nothing new.
  const upcoming: GameView[] =
    match.state === 'decided' || match.state === 'needs_adjudication'
      ? []
      : Array.from({ length: 3 }, (_, index) => index)
          .filter((index) => !match.games.some((g) => g.index === index))
          .map((index) => ({
            index,
            tiebreak: false,
            problem_a: match.order_a?.[index] ?? null,
            problem_b: match.order_b?.[index] ?? null,
            opened_at_ms: 0,
            deadline_ms: 0,
            winner: null,
            decided: false,
          }));
  const rows = [...match.games, ...upcoming];
  const isUpcoming = (g: GameView) => upcoming.includes(g);
  const result = (g: GameView) => {
    if (isUpcoming(g)) {
      return (
        <span className="text-muted-foreground">
          {t('codelink-bracket.game.upcoming')}
        </span>
      );
    }
    if (!g.decided && match.state === 'awaiting_judge') {
      return (
        <span className="text-amber-700 dark:text-amber-300">
          {t('codelink-bracket.game.judging')}
        </span>
      );
    }
    if (!g.decided && match.state === 'needs_adjudication') {
      return (
        <span className="text-red-700 dark:text-red-300">
          {t('codelink-bracket.game.halted')}
        </span>
      );
    }
    if (!g.decided) {
      return (
        <span className="inline-flex items-center gap-1 text-emerald-700 dark:text-emerald-300">
          <Play className="h-3 w-3" />
          {t('codelink-bracket.game.live')}
        </span>
      );
    }
    if (g.winner === null) {
      return (
        <span className="text-muted-foreground">
          {t('codelink-bracket.game.noWinner')}
        </span>
      );
    }
    return (
      <span className="font-medium">
        {g.winner === match.player_a ? nameA : nameB}
      </span>
    );
  };
  return (
    <div className="overflow-hidden rounded-lg border border-border">
      <table className="w-full text-sm">
        <thead className="bg-muted/50 text-xs text-muted-foreground">
          <tr>
            <th className="px-3 py-2 text-left font-medium">
              {t('codelink-bracket.game.column')}
            </th>
            <th className="px-3 py-2 text-left font-medium">{nameA}</th>
            <th className="px-3 py-2 text-left font-medium">{nameB}</th>
            <th className="px-3 py-2 text-left font-medium">
              {t('codelink-bracket.game.winner')}
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((g) => (
            <tr key={g.index} className="border-t border-border">
              <td className="px-3 py-2 text-muted-foreground">{gameName(g)}</td>
              <td className="px-3 py-2">
                <ProblemLink
                  contestId={contestId}
                  problemId={g.problem_a}
                  problems={problems}
                />
              </td>
              <td className="px-3 py-2">
                <ProblemLink
                  contestId={contestId}
                  problemId={g.problem_b}
                  problems={problems}
                />
              </td>
              <td className="px-3 py-2">{result(g)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SubmissionFeed({
  contestId,
  userId,
  name,
  problems,
}: {
  contestId: number;
  userId: number;
  name: string;
  problems: ReadonlyMap<number, ProblemInfo>;
}) {
  const { t } = useTranslation();
  const { data, isLoading, isError } = usePlayerSubmissions(
    contestId,
    userId,
    true,
  );
  return (
    <div className="rounded-lg border border-border">
      <div className="border-b border-border px-3 py-2 text-sm font-medium">
        {name}
      </div>
      <div className="p-1.5">
        {isLoading && (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            {t('codelink-bracket.feed.loading')}
          </p>
        )}
        {isError && (
          <p className="px-2 py-3 text-sm text-destructive">
            {t('codelink-bracket.feed.loadError')}
          </p>
        )}
        {data && data.length === 0 && (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            {t('codelink-bracket.feed.empty')}
          </p>
        )}
        {data && data.length > 0 && (
          <ul>
            {data.map((s) => (
              <li key={s.id}>
                <Link
                  to={submissionPath(contestId, s.id)}
                  className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
                >
                  <span className="w-12 shrink-0 font-mono font-medium">
                    {problems.get(s.problem_id)?.label ?? `#${s.problem_id}`}
                  </span>
                  <VerdictBadge verdict={s.verdict ?? s.status} />
                  <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                    {new Date(s.created_at).toLocaleTimeString()}
                  </span>
                  <span className="font-mono text-xs text-muted-foreground">
                    #{s.id}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function StaffControls({
  match,
  nameA,
  nameB,
  busy,
  onStart,
  onDecide,
}: {
  match: MatchView;
  nameA: string;
  nameB: string;
  busy: boolean;
  onStart: () => void;
  onDecide: (winner: number | null) => void;
}) {
  const { t } = useTranslation();
  const live =
    match.state === 'in_progress' ||
    match.state === 'tiebreak' ||
    match.state === 'awaiting_judge';
  const canAward = live || match.state === 'needs_adjudication';
  if (match.state !== 'ordering' && !canAward) return null;
  const award = (winner: number, name: string) => (
    <ConfirmButton
      size="sm"
      variant={match.state === 'needs_adjudication' ? 'default' : 'outline'}
      disabled={busy}
      title={t('codelink-bracket.staff.awardTitle', { name })}
      description={t('codelink-bracket.staff.awardDescription', { name })}
      confirmLabel={t('codelink-bracket.staff.awardTo', { name })}
      onConfirm={() => onDecide(winner)}
    >
      <Trophy className="mr-1.5 h-3.5 w-3.5" />
      {t('codelink-bracket.staff.awardTo', { name })}
    </ConfirmButton>
  );
  return (
    <Section title={t('codelink-bracket.staff.title')}>
      <div className="flex flex-wrap gap-2 rounded-lg border border-dashed border-border p-3">
        {match.state === 'ordering' && (
          <ConfirmButton
            size="sm"
            variant="outline"
            disabled={busy}
            title={t('codelink-bracket.confirm.startTitle', {
              a: nameA,
              b: nameB,
            })}
            description={t('codelink-bracket.confirm.startDescription')}
            confirmLabel={t('codelink-bracket.staff.start')}
            onConfirm={onStart}
          >
            <Play className="mr-1.5 h-3.5 w-3.5" />
            {busy
              ? t('codelink-bracket.staff.starting')
              : t('codelink-bracket.staff.start')}
          </ConfirmButton>
        )}
        {/* Expiry re-runs the normal decision, which never reopens an
            escalated match -- only an explicit award does. */}
        {live && (
          <ConfirmButton
            size="sm"
            variant="outline"
            disabled={busy}
            title={t('codelink-bracket.confirm.expiryTitle')}
            description={t('codelink-bracket.confirm.expiryDescription')}
            confirmLabel={t('codelink-bracket.staff.forceExpiry')}
            onConfirm={() => onDecide(null)}
          >
            {t('codelink-bracket.staff.forceExpiry')}
          </ConfirmButton>
        )}
        {canAward && award(match.player_a, nameA)}
        {canAward && award(match.player_b, nameB)}
      </div>
    </Section>
  );
}
