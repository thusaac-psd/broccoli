import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button, Label, Switch } from '@broccoli/web-sdk/ui';
import { cn, getUserDisplayName } from '@broccoli/web-sdk/utils';
import { type ReactNode, useState } from 'react';
import { Link } from 'react-router';

import { type RankingMode, rankStandings } from './ranking';
import type { ProblemCell, StandingsResponse } from './types';
import { useCodelinkQuery } from './useCodelinkQuery';

interface Props {
  contestId?: number;
  children?: ReactNode;
}

function formatTime(seconds: number) {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, '0')}`;
}

function Cell({
  cell,
  acceptedView,
}: {
  cell?: ProblemCell;
  acceptedView: boolean;
}) {
  const { t } = useTranslation();
  return (
    <td
      className={cn(
        'border-b border-border px-3 py-2 text-center',
        cell &&
          (acceptedView || cell.status === 'credited') &&
          'bg-emerald-500/10',
        !acceptedView && cell?.status === 'slots_full' && 'bg-amber-500/10',
        !acceptedView &&
          cell?.status === 'after_qualification' &&
          'bg-muted/50',
      )}
    >
      {cell ? (
        <div
          title={t(
            acceptedView
              ? 'codelink-qualifier.cell.accepted'
              : `codelink-qualifier.cell.${cell.status}`,
          )}
        >
          <div className="whitespace-nowrap text-xs font-medium">
            {acceptedView
              ? 'AC'
              : cell.status === 'credited'
                ? t('codelink-qualifier.cell.slot', { slot: cell.slot })
                : t(`codelink-qualifier.cell.${cell.status}`)}
          </div>
          <div className="mt-1 font-mono text-xs text-muted-foreground">
            {formatTime(cell.time_seconds)}
          </div>
        </div>
      ) : (
        <span
          aria-label={t('codelink-qualifier.cell.noAc')}
          className="text-muted-foreground"
        >
          —
        </span>
      )}
    </td>
  );
}

export function CodelinkScoreboard({ contestId, children }: Props) {
  const { t } = useTranslation();
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [rankingMode, setRankingMode] = useState<RankingMode>('points');
  const { data, isError, isPending, isFetching, refetch } =
    useCodelinkQuery<StandingsResponse>(contestId, 'standings', autoRefresh);

  if (!contestId) return <>{children}</>;
  if (isPending)
    return (
      <p className="p-6 text-muted-foreground">
        {t('codelink-qualifier.loading')}
      </p>
    );
  if (!data) {
    return (
      <div role="alert" className="rounded-lg border border-border p-4">
        <p>{t('codelink-qualifier.loadError')}</p>
        <Button
          variant="outline"
          className="mt-2"
          onClick={() => void refetch()}
        >
          {t('codelink-qualifier.refresh')}
        </Button>
      </div>
    );
  }

  const acceptedView = rankingMode === 'accepted';
  const rankedRows = rankStandings(data.rows, rankingMode);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline">
            {t(`codelink-qualifier.phase.${data.phase}`)}
          </Badge>
          <span className="text-sm font-semibold">
            {t('codelink-qualifier.qualifiedCount', {
              count: data.qualified_count,
            })}
          </span>
        </div>
        <div className="flex items-center gap-3">
          {data.scoreboard_refresh_seconds > 0 ? (
            <Label className="flex items-center gap-2 text-sm">
              <Switch checked={autoRefresh} onCheckedChange={setAutoRefresh} />
              {t('codelink-qualifier.autoRefresh', {
                seconds: data.scoreboard_refresh_seconds,
              })}
            </Label>
          ) : (
            <span className="text-sm text-muted-foreground">
              {t('codelink-qualifier.manualRefreshOnly')}
            </span>
          )}
          <Button
            variant="outline"
            size="sm"
            disabled={isFetching}
            onClick={() => void refetch()}
          >
            {t('codelink-qualifier.refresh')}
          </Button>
        </div>
      </div>

      <div
        role="group"
        aria-label={t('codelink-qualifier.ranking.label')}
        className="flex flex-wrap gap-2"
      >
        {(['points', 'accepted'] as const).map((mode) => (
          <Button
            key={mode}
            variant={rankingMode === mode ? 'default' : 'outline'}
            size="sm"
            aria-pressed={rankingMode === mode}
            onClick={() => setRankingMode(mode)}
          >
            {t(`codelink-qualifier.ranking.${mode}`)}
          </Button>
        ))}
      </div>

      {isError && (
        <p role="alert" className="text-sm text-destructive">
          {t('codelink-qualifier.stale')}
        </p>
      )}
      {data.expected_problem_count > 0 &&
        data.problem_count !== data.expected_problem_count && (
          <p role="status" className="rounded-md bg-amber-500/10 p-3 text-sm">
            {t('codelink-qualifier.problemCount', {
              actual: data.problem_count,
              expected: data.expected_problem_count,
            })}
          </p>
        )}
      {data.pending_submissions > 0 && (
        <p role="status" className="rounded-md bg-amber-500/10 p-3 text-sm">
          {t('codelink-qualifier.pending', { count: data.pending_submissions })}
        </p>
      )}

      <section
        hidden={acceptedView}
        aria-label={t('codelink-qualifier.slots.title')}
      >
        <h2 className="mb-2 text-sm font-semibold">
          {t('codelink-qualifier.slots.title')}
        </h2>
        <div className="grid grid-cols-2 gap-2 md:grid-cols-4 xl:grid-cols-8">
          {data.problems.map((problem) => (
            <div
              key={problem.problem_id}
              className="min-w-0 rounded-lg border border-border bg-card p-3"
            >
              <div className="mb-2 flex flex-wrap items-center justify-between gap-1">
                <Link
                  className="font-semibold hover:underline"
                  to={`/contests/${contestId}/problems/${problem.problem_id}`}
                >
                  {problem.label}
                </Link>
                <span className="text-xs text-muted-foreground">
                  {t('codelink-qualifier.slots.remaining', {
                    count: problem.remaining,
                  })}
                </span>
              </div>
              {Array.from({ length: data.slots_per_problem }, (_, slot) => {
                const award = problem.awards[slot];
                return (
                  <div
                    key={slot}
                    className="truncate text-xs leading-6"
                    title={
                      award
                        ? `${getUserDisplayName(award)} (${award.username})`
                        : undefined
                    }
                  >
                    <span className="mr-1 text-muted-foreground">
                      {slot + 1}.
                    </span>
                    {award
                      ? getUserDisplayName(award)
                      : t('codelink-qualifier.slots.available')}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </section>

      <p className="text-sm text-muted-foreground">
        {t(
          acceptedView
            ? 'codelink-qualifier.ranking.acceptedLegend'
            : 'codelink-qualifier.legend',
          {
            slots_per_problem: data.slots_per_problem,
          },
        )}
      </p>
      {data.rows.length === 0 ? (
        <p className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
          {t('codelink-qualifier.empty')}
        </p>
      ) : (
        <div
          key={rankingMode}
          className="overflow-x-auto rounded-lg border border-border"
        >
          <table className="w-full text-sm">
            <caption className="sr-only">
              {t(
                acceptedView
                  ? 'codelink-qualifier.ranking.acceptedCaption'
                  : 'codelink-qualifier.table.caption',
              )}
            </caption>
            <thead className="bg-muted/50">
              <tr>
                {acceptedView && (
                  <th scope="col" className="px-3 py-3 text-center">
                    {t('codelink-qualifier.table.rank')}
                  </th>
                )}
                <th scope="col" className="px-3 py-3 text-left">
                  {t('codelink-qualifier.table.contestant')}
                </th>
                {!acceptedView && (
                  <th scope="col" className="px-3 py-3 text-center">
                    {t('codelink-qualifier.table.status')}
                  </th>
                )}
                <th scope="col" className="px-3 py-3 text-center">
                  {t(
                    acceptedView
                      ? 'codelink-qualifier.table.accepted'
                      : 'codelink-qualifier.table.credited',
                  )}
                </th>
                {!acceptedView && (
                  <th scope="col" className="px-3 py-3 text-center">
                    {t('codelink-qualifier.table.time')}
                  </th>
                )}
                {data.problems.map((p) => (
                  <th
                    key={p.problem_id}
                    scope="col"
                    className="px-3 py-3 text-center"
                  >
                    {p.label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rankedRows.map(({ row, rank }) => (
                <tr key={row.user_id}>
                  {acceptedView && (
                    <td className="border-b border-border px-3 py-3 text-center font-mono">
                      {rank}
                    </td>
                  )}
                  <th
                    scope="row"
                    title={row.username}
                    className="whitespace-nowrap border-b border-border px-3 py-3 text-left font-medium"
                  >
                    {getUserDisplayName(row)}
                  </th>
                  {!acceptedView && (
                    <td className="whitespace-nowrap border-b border-border px-3 py-3 text-center">
                      {row.qualification_verdict ? (
                        <Badge
                          variant={
                            row.qualification_verdict === 'qualified'
                              ? 'default'
                              : 'outline'
                          }
                        >
                          {t(
                            `codelink-qualifier.status.${row.qualification_verdict}`,
                          )}
                        </Badge>
                      ) : (
                        <span
                          aria-label={t('codelink-qualifier.status.noVerdict')}
                          className="text-muted-foreground"
                        >
                          —
                        </span>
                      )}
                    </td>
                  )}
                  <td className="border-b border-border px-3 py-3 text-center font-mono">
                    {acceptedView ? (
                      <div className="font-semibold">{row.accepted}</div>
                    ) : (
                      `${row.credited}/${data.solves_to_qualify}`
                    )}
                  </td>
                  {!acceptedView && (
                    <td className="border-b border-border px-3 py-3 text-center font-mono">
                      {row.qualified_at_seconds === null
                        ? '—'
                        : formatTime(row.qualified_at_seconds)}
                    </td>
                  )}
                  {data.problems.map((p) => (
                    <Cell
                      key={p.problem_id}
                      cell={row.problems[String(p.problem_id)]}
                      acceptedView={acceptedView}
                    />
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
