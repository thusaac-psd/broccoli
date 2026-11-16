import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Label, Switch } from '@broccoli/web-sdk/ui';
import { cn, getUserDisplayName } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import { type ReactNode, useEffect, useState } from 'react';

import { useIcpcApi } from './hooks/useIcpcApi';
import { useIsIcpcContest } from './hooks/useIsIcpcContest';
import type { ProblemCell, StandingsEntry, StandingsResponse } from './types';

interface IcpcScoreboardProps {
  contestId?: number;
  children?: ReactNode;
}

const MEDAL_COLORS = ['#D4AF37', '#A8A8A8', '#CD7F32'] as const;

// Inline lucide "lock" glyph - a real vector icon (no emoji), no extra dep so the
// plugin bundle stays self-contained.
function LockIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      <rect width="18" height="11" x="3" y="11" rx="2" ry="2" />
      <path d="M7 11V7a5 5 0 0 1 10 0v4" />
    </svg>
  );
}

function PhaseBar({ phase, frozen }: { phase: string; frozen?: boolean }) {
  const { t } = useTranslation();
  // While frozen, the freeze state replaces the phase (e.g. "Frozen" instead of
  // "In progress"), since a frozen board is the salient status for the viewer.
  if (frozen) {
    return (
      <div className="flex items-center gap-2 py-2 px-4 rounded-md text-[13px] font-medium bg-amber-500/10 text-amber-600">
        <LockIcon />
        {t('icpc.scoreboard.frozen')}
      </div>
    );
  }
  return (
    <div
      className={cn(
        'flex items-center gap-2 py-2 px-4 rounded-md text-[13px] font-medium',
        phase === 'before' && 'bg-blue-500/10 text-blue-500',
        phase === 'during' && 'bg-emerald-500/10 text-emerald-500',
        phase !== 'before' &&
          phase !== 'during' &&
          'bg-gray-500/10 text-gray-500',
      )}
    >
      <span
        className={cn(
          'w-2 h-2 rounded-full',
          phase === 'before'
            ? 'bg-blue-500'
            : phase === 'during'
              ? 'bg-emerald-500'
              : 'bg-gray-500',
        )}
      />
      {phase === 'before'
        ? t('icpc.scoreboard.phase.before')
        : phase === 'during'
          ? t('icpc.scoreboard.phase.during')
          : t('icpc.scoreboard.phase.after')}
    </div>
  );
}

function MedalBadge({ rank }: { rank: number }) {
  if (rank > 3) {
    return (
      <span className="font-mono tabular-nums text-[13px] text-muted-foreground">
        {rank}
      </span>
    );
  }
  const color = MEDAL_COLORS[rank - 1];
  return (
    <span
      className="inline-flex items-center justify-center w-6 h-6 rounded-full text-white text-xs font-bold"
      style={{ background: color }}
    >
      {rank}
    </span>
  );
}

function ProblemCellView({ cell }: { cell: ProblemCell | undefined }) {
  if (!cell) {
    // No attempts
    return (
      <td className="py-1.5 px-2 text-center border-b border-border">
        <span className="text-[11px] text-muted-foreground/30">&mdash;</span>
      </td>
    );
  }

  if (cell.solved) {
    return (
      <td
        className={cn(
          'py-1.5 px-2 text-center border-b border-border',
          cell.first_solve ? 'bg-emerald-500/20' : 'bg-emerald-500/10',
        )}
      >
        <div
          className={cn(
            'font-mono text-[13px] leading-tight',
            cell.first_solve
              ? 'font-bold text-emerald-600'
              : 'font-semibold text-emerald-700',
          )}
        >
          {cell.attempts > 0 ? `+${cell.attempts}` : '+'}
        </div>
        <div className="font-mono text-[10px] text-muted-foreground">
          {cell.time}
        </div>
      </td>
    );
  }

  // Pending: submissions during the freeze, verdict hidden. Two lines like the
  // solved cell - "?" on top, then the pre-freeze wrong attempts and the frozen
  // count below: "-3 (4)" (3 wrong before, 4 pending during), or "(4)" if none.
  if (cell.pending && cell.pending > 0) {
    return (
      <td className="py-1.5 px-2 text-center border-b border-border bg-amber-500/10">
        <div className="font-mono text-[13px] font-bold text-amber-600 leading-tight">
          ?
        </div>
        <div className="font-mono text-[10px] text-muted-foreground leading-tight">
          {cell.attempts > 0 && (
            <span className="text-red-600">-{cell.attempts} </span>
          )}
          ({cell.pending})
        </div>
      </td>
    );
  }

  // Attempted but unsolved
  return (
    <td className="py-1.5 px-2 text-center border-b border-border bg-red-500/8">
      <div className="font-mono text-[13px] font-semibold text-red-600 leading-tight">
        -{cell.attempts}
      </div>
    </td>
  );
}

export function IcpcScoreboard({ contestId, children }: IcpcScoreboardProps) {
  const { isIcpc, isLoading: guardLoading } = useIsIcpcContest(contestId);
  const api = useIcpcApi();
  const { t } = useTranslation();
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [now, setNow] = useState(Date.now());
  const [revealing, setRevealing] = useState(false);
  const [revealError, setRevealError] = useState<string | null>(null);

  const {
    data: standings,
    isLoading,
    isError,
    dataUpdatedAt,
    refetch,
  } = useQuery<StandingsResponse>({
    queryKey: ['icpc-standings', contestId],
    enabled: !!contestId && isIcpc,
    queryFn: () => api.getStandings(contestId!),
    retry: 2,
    refetchInterval: (query: { state: { data?: StandingsResponse } }) => {
      const d = query.state.data;
      // Poll during the contest, and also while a frozen board is awaiting reveal
      // (the 'after' phase) so the reveal reaches viewers who leave the tab open.
      const live = d?.phase === 'during' || (!!d?.frozen && !d?.revealed);
      return autoRefresh && live ? 30000 : false;
    },
  });

  const handleReveal = async () => {
    if (!contestId) return;
    // Reveal is final and cannot be undone from the UI - confirm first.
    if (!window.confirm(t('icpc.scoreboard.revealConfirm'))) return;
    setRevealing(true);
    setRevealError(null);
    try {
      await api.reveal(contestId);
      await refetch();
    } catch (e) {
      // Surface the failure - do NOT let the organizer believe it succeeded.
      setRevealError(e instanceof Error ? e.message : String(e));
    } finally {
      setRevealing(false);
    }
  };

  const hasData = dataUpdatedAt > 0;
  useEffect(() => {
    if (!hasData) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [hasData]);

  if (guardLoading || !isIcpc) return <>{children}</>;

  if (isError && !standings) {
    return (
      <div className="p-6 text-center rounded-md bg-red-500/[0.06] text-red-600 text-[13px]">
        {t('icpc.scoreboard.loadError')}
      </div>
    );
  }

  if (isLoading || !standings) {
    return (
      <div className="p-6 text-center text-muted-foreground">
        {t('icpc.scoreboard.loading')}
      </div>
    );
  }

  const { phase, frozen, can_reveal, problem_labels, rows } = standings;

  const secondsAgo =
    dataUpdatedAt > 0 ? Math.floor((now - dataUpdatedAt) / 1000) : 0;

  return (
    <div>
      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <PhaseBar phase={phase} frozen={frozen} />
        <div className="flex items-center gap-3">
          {revealError && (
            <span className="text-[11px] text-red-600" title={revealError}>
              {t('icpc.scoreboard.revealFailed')}
            </span>
          )}
          {can_reveal && (
            <button
              type="button"
              onClick={handleReveal}
              disabled={revealing}
              className="rounded-md bg-amber-500 px-3 py-1 text-[12px] font-semibold text-white hover:bg-amber-600 disabled:opacity-50"
            >
              {revealing
                ? t('icpc.scoreboard.revealing')
                : t('icpc.scoreboard.reveal')}
            </button>
          )}
          {dataUpdatedAt > 0 && (
            <span className="text-[11px] opacity-50">
              {t('icpc.scoreboard.lastUpdated', { seconds: secondsAgo })}
            </span>
          )}
          {phase === 'during' && (
            <Label className="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer font-normal">
              <Switch checked={autoRefresh} onCheckedChange={setAutoRefresh} />
              {t('icpc.scoreboard.autoRefresh')}
            </Label>
          )}
        </div>
      </div>

      {rows.length === 0 ? (
        <div className="py-12 text-center text-muted-foreground">
          {phase === 'before'
            ? t('icpc.scoreboard.empty.before')
            : phase === 'during'
              ? t('icpc.scoreboard.empty.during')
              : t('icpc.scoreboard.empty.after')}
        </div>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className="w-full border-collapse min-w-[500px]">
            <thead>
              <tr>
                <th
                  className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                  style={{ width: 50 }}
                >
                  {t('icpc.scoreboard.header.rank')}
                </th>
                <th className="py-2 px-3 text-left font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap">
                  {t('icpc.scoreboard.header.team')}
                </th>
                <th
                  className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                  style={{ width: 60 }}
                >
                  {t('icpc.scoreboard.header.solved')}
                </th>
                <th
                  className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                  style={{ width: 70 }}
                >
                  {t('icpc.scoreboard.header.penalty')}
                </th>
                {problem_labels.map((label) => (
                  <th
                    key={label}
                    className="py-2 px-2 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                    style={{ width: 65 }}
                  >
                    {label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((entry) => (
                <RankRow
                  key={entry.user_id}
                  entry={entry}
                  problemLabels={problem_labels}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function RankRow({
  entry,
  problemLabels,
}: {
  entry: StandingsEntry;
  problemLabels: string[];
}) {
  return (
    <tr className="transition-colors duration-150 hover:bg-muted/50">
      <td
        className="py-1.5 px-3 border-b border-border text-center"
        style={{ width: 50 }}
      >
        <MedalBadge rank={entry.rank} />
      </td>
      <td
        className="py-1.5 px-3 border-b border-border text-[13px] font-medium"
        title={entry.username}
      >
        {getUserDisplayName(entry)}
      </td>
      <td
        className="py-1.5 px-3 border-b border-border text-center font-bold font-mono tabular-nums text-[14px]"
        style={{ width: 60 }}
      >
        {entry.solved}
      </td>
      <td
        className="py-1.5 px-3 border-b border-border text-center font-mono tabular-nums text-[13px] text-muted-foreground"
        style={{ width: 70 }}
      >
        {entry.penalty}
      </td>
      {problemLabels.map((label) => (
        <ProblemCellView key={label} cell={entry.problems[label]} />
      ))}
    </tr>
  );
}
