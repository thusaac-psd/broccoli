import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Label, Switch } from '@broccoli/web-sdk/ui';
import { cn, getUserDisplayName } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import { type ReactNode, useEffect, useRef, useState } from 'react';

import { useIoiApi } from './hooks/useIoiApi';
import { useIsIoiContest } from './hooks/useIsIoiContest';
import type { ScoreboardEntry, ScoreboardResponse } from './types';

interface IoiScoreboardProps {
  contestId?: number;
  children?: ReactNode;
}

const MEDAL_COLORS = ['#D4AF37', '#A8A8A8', '#CD7F32'] as const;

function scoreColor(score: number, maxPossible: number): string {
  if (maxPossible <= 0) return 'transparent';
  const frac = score / maxPossible;
  if (frac >= 1) return 'rgba(16, 185, 129, 0.25)';
  if (frac > 0) return `rgba(245, 158, 11, ${0.08 + frac * 0.17})`;
  return 'transparent';
}

function scoreBorderColor(score: number, maxPossible: number): string {
  if (maxPossible <= 0) return 'transparent';
  const frac = score / maxPossible;
  if (frac >= 1) return 'rgba(16, 185, 129, 0.5)';
  if (frac > 0) return 'rgba(245, 158, 11, 0.3)';
  return 'transparent';
}

function formatElapsedTime(seconds: number): string {
  const totalMinutes = Math.floor(Math.max(0, seconds) / 60);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) {
    return `${hours}h ${String(minutes).padStart(2, '0')}m`;
  }
  return `${totalMinutes}m`;
}

function PhaseBar({ phase }: { phase: string }) {
  const { t } = useTranslation();
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
        ? t('ioi.scoreboard.phase.before')
        : phase === 'during'
          ? t('ioi.scoreboard.phase.during')
          : t('ioi.scoreboard.phase.after')}
    </div>
  );
}

function PhaseBanner({
  type,
  onDismiss,
}: {
  type: 'started' | 'ended';
  onDismiss: () => void;
}) {
  const { t } = useTranslation();
  const isEnded = type === 'ended';
  return (
    <div
      className={cn(
        'flex items-center justify-between py-2 px-3.5 rounded-md mb-3 text-[13px] font-medium border',
        isEnded
          ? 'bg-amber-500/10 text-amber-700 border-amber-500/20'
          : 'bg-emerald-500/10 text-emerald-600 border-emerald-500/20',
      )}
    >
      <span>
        {isEnded
          ? t('ioi.scoreboard.phaseChange.ended')
          : t('ioi.scoreboard.phaseChange.started')}
      </span>
      <button
        onClick={onDismiss}
        aria-label="Dismiss"
        className="border-none bg-transparent cursor-pointer opacity-50 text-inherit p-0 px-1 leading-none flex items-center"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
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

function ScoreCell({ score, max }: { score: number; max: number }) {
  return (
    <td
      className="py-1.5 px-3 text-center border-b border-border relative"
      style={{ background: scoreColor(score, max) }}
    >
      <div
        className="absolute left-0 top-0 bottom-0 w-[3px]"
        style={{ background: scoreBorderColor(score, max) }}
      />
      <span
        className={cn(
          'font-mono tabular-nums text-[13px]',
          score > 0
            ? 'font-semibold text-foreground'
            : 'font-normal text-muted-foreground',
        )}
      >
        {score.toFixed(score === Math.floor(score) ? 0 : 2)}
      </span>
    </td>
  );
}

export function IoiScoreboard({ contestId, children }: IoiScoreboardProps) {
  const { isIoi, isLoading: guardLoading } = useIsIoiContest(contestId);
  const api = useIoiApi();
  const { t } = useTranslation();
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [phaseBanner, setPhaseBanner] = useState<'started' | 'ended' | null>(
    null,
  );
  const previousPhaseRef = useRef<string | null>(null);
  const [now, setNow] = useState(Date.now());

  const {
    data: scoreboard,
    isLoading,
    isError,
    dataUpdatedAt,
  } = useQuery<ScoreboardResponse>({
    queryKey: ['ioi-scoreboard', contestId],
    enabled: !!contestId && isIoi,
    queryFn: () => api.getScoreboard(contestId!),
    retry: 2,
    refetchInterval: (query: { state: { data?: ScoreboardResponse } }) =>
      autoRefresh && query.state.data?.phase === 'during' ? 30000 : false,
  });

  useEffect(() => {
    const currentPhase = scoreboard?.phase ?? null;
    const prevPhase = previousPhaseRef.current;
    previousPhaseRef.current = currentPhase;

    if (currentPhase && prevPhase && currentPhase !== prevPhase) {
      const banner =
        currentPhase === 'after'
          ? 'ended'
          : currentPhase === 'during'
            ? 'started'
            : null;
      if (banner) {
        setPhaseBanner(banner);
        const timer = setTimeout(() => setPhaseBanner(null), 10000);
        return () => clearTimeout(timer);
      }
    }
  }, [scoreboard?.phase]);

  useEffect(() => {
    if (!dataUpdatedAt) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [dataUpdatedAt]);

  if (guardLoading || !isIoi) return <>{children}</>;

  if (isError && !scoreboard) {
    return (
      <div className="p-6 text-center rounded-md bg-red-500/[0.06] text-red-600 text-[13px]">
        {t('ioi.scoreboard.loadError')}
      </div>
    );
  }

  if (isLoading || !scoreboard) {
    return (
      <div className="p-6 text-center text-muted-foreground">
        {t('ioi.scoreboard.loading')}
      </div>
    );
  }

  const { phase, rankings } = scoreboard;
  const showTimeTiebreaker = scoreboard.scoreboard_tiebreaker !== 'equal_rank';

  // Determine problem labels from the first entry that has problems
  const sampleEntry = rankings.find(
    (r: ScoreboardEntry) => r.problems && r.problems.length > 0,
  );
  const problemIds =
    sampleEntry?.problems?.map((p: { problem_id: number }) => p.problem_id) ??
    [];
  const problemLabels = problemIds.map((_: number, i: number) =>
    String.fromCharCode(65 + i),
  );

  // Per-problem max scores from backend (sum of subtask max_scores or test case scores)
  const maxPerProblem: Record<number, number> = {};
  for (const pid of problemIds) {
    maxPerProblem[pid] = scoreboard.max_scores?.[String(pid)] ?? 100;
  }
  const maxTotal =
    Object.values(maxPerProblem).reduce((sum, v) => sum + v, 0) || 1;

  const secondsAgo =
    dataUpdatedAt > 0 ? Math.floor((now - dataUpdatedAt) / 1000) : 0;

  return (
    <div>
      {phaseBanner && (
        <PhaseBanner
          type={phaseBanner}
          onDismiss={() => setPhaseBanner(null)}
        />
      )}

      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <PhaseBar phase={phase} />
        <div className="flex items-center gap-3">
          {dataUpdatedAt > 0 && (
            <span className="text-[11px] opacity-50">
              {t('ioi.scoreboard.lastUpdated', { seconds: secondsAgo })}
            </span>
          )}
          {phase === 'during' && (
            <Label className="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer font-normal">
              <Switch checked={autoRefresh} onCheckedChange={setAutoRefresh} />
              {t('ioi.scoreboard.autoRefresh')}
            </Label>
          )}
        </div>
      </div>

      {phase === 'during' &&
        scoreboard.scoreboard_visibility === 'admins_only' && (
          <div className="py-1.5 px-3 mb-3 rounded text-xs text-muted-foreground bg-muted border border-border">
            {t('ioi.scoreboard.ownScoresOnly')}
          </div>
        )}
      {phase === 'during' &&
        scoreboard.scoreboard_visibility === 'all_contest_viewers' && (
          <div className="py-1.5 px-3 mb-3 rounded text-xs text-muted-foreground bg-muted border border-border">
            {t('ioi.scoreboard.fullLiveVisible')}
          </div>
        )}

      {rankings.length === 0 ? (
        <div className="py-12 text-center text-muted-foreground">
          {phase === 'before'
            ? t('ioi.scoreboard.empty.before')
            : phase === 'during'
              ? t('ioi.scoreboard.empty.during')
              : t('ioi.scoreboard.empty.after')}
        </div>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className="w-full border-collapse min-w-[500px]">
            <thead>
              <tr>
                <th
                  className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap sticky z-[1]"
                  style={{ left: 0, width: 50 }}
                >
                  {t('ioi.scoreboard.header.rank')}
                </th>
                <th
                  className="py-2 px-3 text-left font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap sticky z-[1]"
                  style={{ left: 50 }}
                >
                  {t('ioi.scoreboard.header.user')}
                </th>
                {problemLabels.map((label: string, i: number) => (
                  <th
                    key={problemIds[i]}
                    className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                    style={{ width: 80 }}
                  >
                    {label}
                  </th>
                ))}
                {showTimeTiebreaker && (
                  <th
                    className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                    style={{ width: 90 }}
                  >
                    {t('ioi.scoreboard.header.time')}
                  </th>
                )}
                <th
                  className="py-2 px-3 text-center font-semibold text-xs uppercase tracking-wide text-muted-foreground border-b-2 border-border bg-muted whitespace-nowrap"
                  style={{ width: 90 }}
                >
                  {t('ioi.scoreboard.header.total')}
                </th>
              </tr>
            </thead>
            <tbody>
              {rankings.map((entry: ScoreboardEntry) => (
                <RankRow
                  key={entry.user_id}
                  entry={entry}
                  problemIds={problemIds}
                  maxPerProblem={maxPerProblem}
                  maxTotal={maxTotal}
                  showTimeTiebreaker={showTimeTiebreaker}
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
  problemIds,
  maxPerProblem,
  maxTotal,
  showTimeTiebreaker,
}: {
  entry: ScoreboardEntry;
  problemIds: number[];
  maxPerProblem: Record<number, number>;
  maxTotal: number;
  showTimeTiebreaker: boolean;
}) {
  const problemScoreMap: Record<number, number> = {};
  if (entry.problems) {
    for (const p of entry.problems) {
      problemScoreMap[p.problem_id] = p.score;
    }
  }

  return (
    <tr className="transition-colors duration-150">
      <td
        className="py-1.5 px-3 border-b border-border text-[13px] text-center sticky z-[1] bg-inherit"
        style={{ left: 0, width: 50 }}
      >
        <MedalBadge rank={entry.rank} />
      </td>
      <td
        className="py-1.5 px-3 border-b border-border text-[13px] font-medium sticky z-[1] bg-inherit"
        style={{ left: 50 }}
        title={entry.username}
      >
        {getUserDisplayName(entry)}
      </td>
      {problemIds.map((pid) => {
        const score = problemScoreMap[pid] ?? 0;
        const max = maxPerProblem[pid] ?? 100;
        return <ScoreCell key={pid} score={score} max={max} />;
      })}
      {showTimeTiebreaker && (
        <td className="py-1.5 px-3 text-center font-mono tabular-nums text-[13px] border-b border-border text-muted-foreground whitespace-nowrap">
          {formatElapsedTime(entry.total_time_seconds)}
        </td>
      )}
      <td
        className="py-1.5 px-3 text-center font-bold font-mono tabular-nums border-b border-border"
        style={{ background: scoreColor(entry.total_score, maxTotal) }}
      >
        {entry.total_score.toFixed(
          entry.total_score === Math.floor(entry.total_score) ? 0 : 2,
        )}
      </td>
    </tr>
  );
}
