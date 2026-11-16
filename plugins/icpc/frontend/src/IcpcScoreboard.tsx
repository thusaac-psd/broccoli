import { serverNow } from '@broccoli/web-sdk/api';
import { contestPollPace } from '@broccoli/web-sdk/contest';
import { pollInterval, unsettledMessageKey } from '@broccoli/web-sdk/hooks';
import { formatRelativeSeconds, useTranslation } from '@broccoli/web-sdk/i18n';
import { useContestSlotContext } from '@broccoli/web-sdk/slot';
import {
  Label,
  QueryView,
  Switch,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { cn, getUserDisplayName } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import { type ReactNode, useEffect, useState } from 'react';

import { useIcpcApi } from './hooks/useIcpcApi';
import type { ProblemCell, StandingsEntry, StandingsResponse } from './types';

/** How often a live board asks for new results. */
const LIVE_BOARD_POLL_MS = 30_000;

interface IcpcScoreboardProps {
  contestId?: number;
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
      <div className="flex items-center gap-2 py-2 px-4 rounded-md text-[13px] font-medium bg-muted text-foreground">
        <LockIcon />
        {t('icpc.scoreboard.frozen')}
      </div>
    );
  }
  return (
    <div
      className={cn(
        // The dot carries the color; the text stays on the contrast-tuned
        // tokens.
        'flex items-center gap-2 py-2 px-4 rounded-md text-[13px] font-medium bg-muted text-foreground',
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

function MedalBadge({ rank }: { rank: number | null }) {
  // A board restricted to the viewer's own row carries no rank.
  if (rank === null) {
    return <span className="text-[13px] text-muted-foreground">—</span>;
  }
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
        <span className="text-[11px] text-muted-foreground">&mdash;</span>
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
              ? 'font-bold text-emerald-800 dark:text-emerald-400'
              : 'font-semibold text-emerald-700 dark:text-emerald-400',
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
        <div className="font-mono text-[13px] font-bold text-amber-800 dark:text-amber-400 leading-tight">
          ?
        </div>
        <div className="font-mono text-[10px] text-muted-foreground leading-tight">
          {cell.attempts > 0 && (
            <span className="text-destructive">-{cell.attempts} </span>
          )}
          ({cell.pending})
        </div>
      </td>
    );
  }

  // Attempted but unsolved
  return (
    <td className="py-1.5 px-2 text-center border-b border-border bg-red-500/8">
      <div className="font-mono text-[13px] font-semibold text-destructive leading-tight">
        -{cell.attempts}
      </div>
    </td>
  );
}

// Mounted only on ICPC contests: the slot declares `contest_type = "icpc"`.
export function IcpcScoreboard({ contestId }: IcpcScoreboardProps) {
  const api = useIcpcApi();
  const contest = useContestSlotContext()?.contest ?? null;
  const { t, locale } = useTranslation();
  const [autoRefresh, setAutoRefresh] = useState(true);
  // eslint-disable-next-line no-restricted-properties -- see the tick below
  const [now, setNow] = useState(Date.now());
  const [confirm, confirmDialog] = useConfirmDialog();

  const query = useQuery<StandingsResponse>({
    queryKey: ['icpc-standings', contestId],
    enabled: contestId !== undefined,
    queryFn: () => api.getStandings(contestId!),
    retry: 2,
    refetchInterval: pollInterval<StandingsResponse>((d) => {
      // A board refreshes while the server says it may still change
      // (`live`: the contest runs, a submission is still being judged, or a
      // frozen board awaits its reveal, so the reveal reaches viewers who
      // leave the tab open); every board also turns over when the contest's
      // phase does (`contestPollPace`).
      if (!autoRefresh || d === undefined) return false;
      const pace = contest ? contestPollPace(contest, serverNow()) : false;
      if (!d.live) return pace;
      return Math.min(LIVE_BOARD_POLL_MS, pace === false ? Infinity : pace);
    }),
  });
  const { dataUpdatedAt, refetch } = query;

  // Reveal is final and cannot be undone from the UI, so it is confirmed
  // first. A failure stays in the dialog: the organizer never believes it
  // succeeded.
  const handleReveal = () => {
    if (!contestId) return;
    confirm({
      title: t('icpc.scoreboard.revealConfirm'),
      confirmLabel: t('icpc.scoreboard.reveal'),
      destructive: true,
      failureMessage: t('icpc.scoreboard.revealFailed'),
      onConfirm: async () => {
        const result = await api.reveal(contestId);
        await refetch();
        // It may or may not be revealed: the refetched board shows which.
        if (!result.settled) {
          throw new Error(t(unsettledMessageKey(result.outcome)));
        }
      },
    });
  };

  const hasData = dataUpdatedAt > 0;
  useEffect(() => {
    if (!hasData) return;
    // `dataUpdatedAt` is stamped by this machine's clock, so its age is too.
    // eslint-disable-next-line no-restricted-properties
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [hasData]);

  // `now` ticks behind `dataUpdatedAt` right after a refetch; never read
  // that as a time in the future.
  const updated = formatRelativeSeconds(
    Math.min(0, (dataUpdatedAt - now) / 1000),
    locale,
  );

  return (
    <QueryView
      query={query}
      loading={
        <div className="p-6 text-center text-muted-foreground">
          {t('icpc.scoreboard.loading')}
        </div>
      }
      failure={t('icpc.scoreboard.loadError')}
    >
      {(standings) => (
        <Standings
          standings={standings}
          updated={updated}
          autoRefresh={autoRefresh}
          setAutoRefresh={setAutoRefresh}
          onReveal={handleReveal}
          confirmDialog={confirmDialog}
        />
      )}
    </QueryView>
  );
}

function Standings({
  standings,
  updated,
  autoRefresh,
  setAutoRefresh,
  onReveal,
  confirmDialog,
}: {
  standings: StandingsResponse;
  updated: string;
  autoRefresh: boolean;
  setAutoRefresh: (on: boolean) => void;
  onReveal: () => void;
  confirmDialog: ReactNode;
}) {
  const { t } = useTranslation();
  const { phase, frozen, can_reveal, problem_labels, rows } = standings;

  // The board never widens its container: a narrow screen scrolls the
  // table inside its border instead of clipping its last columns.
  return (
    <div className="min-w-0 max-w-full">
      {confirmDialog}
      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <PhaseBar phase={phase} frozen={frozen} />
        <div className="flex items-center gap-3">
          {can_reveal && (
            <button
              type="button"
              onClick={onReveal}
              className="rounded-md bg-amber-500 px-3 py-1 text-[12px] font-semibold text-white hover:bg-amber-600 disabled:opacity-50"
            >
              {t('icpc.scoreboard.reveal')}
            </button>
          )}
          <span className="text-[11px] text-muted-foreground">
            {t('icpc.scoreboard.lastUpdated', { when: updated })}
          </span>
          {phase === 'during' && (
            <Label className="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer font-normal">
              <Switch
                checked={autoRefresh}
                onCheckedChange={setAutoRefresh}
                aria-label={t('icpc.scoreboard.autoRefresh')}
              />
              {t('icpc.scoreboard.autoRefresh')}
            </Label>
          )}
        </div>
      </div>

      {rows.length === 0 ? (
        <div className="py-12 text-center text-muted-foreground">
          {!standings.full_board
            ? t('icpc.scoreboard.hidden')
            : phase === 'before'
              ? t('icpc.scoreboard.empty.before')
              : phase === 'during'
                ? t('icpc.scoreboard.empty.during')
                : t('icpc.scoreboard.empty.after')}
        </div>
      ) : (
        <div
          className="overflow-x-auto rounded-lg border border-border"
          // Scrolls sideways on a narrow screen: reachable by keyboard.
          tabIndex={0}
          role="region"
          aria-label={t('icpc.scoreboard.table')}
        >
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
