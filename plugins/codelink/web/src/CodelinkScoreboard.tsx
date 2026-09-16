import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button, Label, Switch } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { type ReactNode, useState } from 'react';
import { Link } from 'react-router';

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

function Cell({ cell }: { cell?: ProblemCell }) {
  const { t } = useTranslation();
  return (
    <td
      className={cn(
        'border-b border-border px-3 py-2 text-center',
        cell?.status === 'credited' && 'bg-emerald-500/10',
        cell?.status === 'slots_full' && 'bg-amber-500/10',
        cell?.status === 'after_qualification' && 'bg-muted/50',
      )}
    >
      {cell ? (
        <div title={t(`codelink.cell.${cell.status}`)}>
          <div className="whitespace-nowrap text-xs font-medium">
            {cell.status === 'credited'
              ? t('codelink.cell.slot', { slot: cell.slot })
              : t(`codelink.cell.${cell.status}`)}
          </div>
          <div className="mt-1 font-mono text-xs text-muted-foreground">
            {formatTime(cell.time_seconds)}
          </div>
        </div>
      ) : (
        <span
          aria-label={t('codelink.cell.noAc')}
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
  const { data, isError, isPending, isFetching, refetch } =
    useCodelinkQuery<StandingsResponse>(contestId, 'standings', autoRefresh);

  if (!contestId) return <>{children}</>;
  if (isPending)
    return <p className="p-6 text-muted-foreground">{t('codelink.loading')}</p>;
  if (!data) {
    return (
      <div role="alert" className="rounded-lg border border-border p-4">
        <p>{t('codelink.loadError')}</p>
        <Button
          variant="outline"
          className="mt-2"
          onClick={() => void refetch()}
        >
          {t('codelink.refresh')}
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline">{t(`codelink.phase.${data.phase}`)}</Badge>
          <span className="text-sm font-semibold">
            {t('codelink.qualifiedCount', {
              count: data.qualified_count,
            })}
          </span>
        </div>
        <div className="flex items-center gap-3">
          {data.scoreboard_refresh_seconds > 0 ? (
            <Label className="flex items-center gap-2 text-sm">
              <Switch checked={autoRefresh} onCheckedChange={setAutoRefresh} />
              {t('codelink.autoRefresh', {
                seconds: data.scoreboard_refresh_seconds,
              })}
            </Label>
          ) : (
            <span className="text-sm text-muted-foreground">
              {t('codelink.manualRefreshOnly')}
            </span>
          )}
          <Button
            variant="outline"
            size="sm"
            disabled={isFetching}
            onClick={() => void refetch()}
          >
            {t('codelink.refresh')}
          </Button>
        </div>
      </div>

      {isError && (
        <p role="alert" className="text-sm text-destructive">
          {t('codelink.stale')}
        </p>
      )}
      {data.expected_problem_count > 0 &&
        data.problem_count !== data.expected_problem_count && (
          <p role="status" className="rounded-md bg-amber-500/10 p-3 text-sm">
            {t('codelink.problemCount', {
              actual: data.problem_count,
              expected: data.expected_problem_count,
            })}
          </p>
        )}
      {data.pending_submissions > 0 && (
        <p role="status" className="rounded-md bg-amber-500/10 p-3 text-sm">
          {t('codelink.pending', { count: data.pending_submissions })}
        </p>
      )}

      <section aria-label={t('codelink.slots.title')}>
        <h2 className="mb-2 text-sm font-semibold">
          {t('codelink.slots.title')}
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
                  {t('codelink.slots.remaining', { count: problem.remaining })}
                </span>
              </div>
              {Array.from({ length: data.slots_per_problem }, (_, slot) => {
                const award = problem.awards[slot];
                return (
                  <div
                    key={slot}
                    className="truncate text-xs leading-6"
                    title={award?.username}
                  >
                    <span className="mr-1 text-muted-foreground">
                      {slot + 1}.
                    </span>
                    {award?.username ?? t('codelink.slots.available')}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </section>

      <p className="text-sm text-muted-foreground">
        {t('codelink.legend', { slots_per_problem: data.slots_per_problem })}
      </p>
      {data.rows.length === 0 ? (
        <p className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
          {t('codelink.empty')}
        </p>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className="w-full text-sm">
            <caption className="sr-only">{t('codelink.table.caption')}</caption>
            <thead className="bg-muted/50">
              <tr>
                <th scope="col" className="px-3 py-3 text-left">
                  {t('codelink.table.contestant')}
                </th>
                <th scope="col" className="px-3 py-3 text-center">
                  {t('codelink.table.status')}
                </th>
                <th scope="col" className="px-3 py-3 text-center">
                  {t('codelink.table.credited')}
                </th>
                <th scope="col" className="px-3 py-3 text-center">
                  {t('codelink.table.time')}
                </th>
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
              {data.rows.map((row) => (
                <tr key={row.user_id}>
                  <th
                    scope="row"
                    className="whitespace-nowrap border-b border-border px-3 py-3 text-left font-medium"
                  >
                    {row.username}
                  </th>
                  <td className="whitespace-nowrap border-b border-border px-3 py-3 text-center">
                    {row.qualification_verdict ? (
                      <Badge
                        variant={
                          row.qualification_verdict === 'qualified'
                            ? 'default'
                            : 'outline'
                        }
                      >
                        {t(`codelink.status.${row.qualification_verdict}`)}
                      </Badge>
                    ) : (
                      <span
                        aria-label={t('codelink.status.noVerdict')}
                        className="text-muted-foreground"
                      >
                        —
                      </span>
                    )}
                  </td>
                  <td className="border-b border-border px-3 py-3 text-center font-mono">
                    {row.credited}/{data.solves_to_qualify}
                  </td>
                  <td className="border-b border-border px-3 py-3 text-center font-mono">
                    {row.qualified_at_seconds === null
                      ? '—'
                      : formatTime(row.qualified_at_seconds)}
                  </td>
                  {data.problems.map((p) => (
                    <Cell
                      key={p.problem_id}
                      cell={row.problems[String(p.problem_id)]}
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
