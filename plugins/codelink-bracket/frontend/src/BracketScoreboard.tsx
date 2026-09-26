import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Button, Skeleton } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import {
  AlertTriangle,
  Crown,
  Maximize2,
  Minimize2,
  Trophy,
} from 'lucide-react';
import {
  type ReactNode,
  type RefObject,
  useEffect,
  useRef,
  useState,
} from 'react';

import { useBracketApi } from './hooks/useBracketApi';
import { gamePips, type Pip, seedsFrom } from './lib/pips';
import { playerLabel } from './lib/player';
import {
  matchesInRound,
  ROUND_COUNT,
  roundNameKey,
  roundShortKey,
} from './lib/rounds';
import { sideOf } from './lib/stage';
import { MatchSheet } from './MatchSheet';
import { MyMatchStrip } from './MyMatchStrip';
import { StatusPill, useGameClock, useStartsIn } from './parts';
import { SetupPanel } from './SetupPanel';
import type { MatchView } from './types';

interface BracketScoreboardProps {
  contestId?: number;
  contestType?: string;
  children?: ReactNode;
}

const needsAttention = (m: MatchView) =>
  m.state === 'awaiting_judge' || m.state === 'needs_adjudication';
const isLive = (m: MatchView) =>
  m.state === 'in_progress' || m.state === 'tiebreak';

/**
 * The ranking page for the afternoon round: the viewer's own match first (for
 * players), then the whole bracket as a tree. Registered at `ranking.content`.
 * Every match here is already visibility-filtered by `GET /bracket`.
 */
export function BracketScoreboard({
  contestId,
  children,
}: BracketScoreboardProps) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const auth = useAuth();
  const [openMatch, setOpenMatch] = useState<number | null>(null);
  const presentRef = useRef<HTMLDivElement>(null);
  const presenting = useFullscreen(presentRef);

  const { data, isLoading, isError } = useQuery({
    queryKey: ['codelink-bracket-bracket', contestId],
    enabled: !!contestId,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval: 5_000,
  });

  if (!contestId) return <>{children}</>;

  if (isError) {
    return (
      <div className="rounded-lg border border-red-500/30 bg-red-500/5 p-6 text-center text-sm text-red-700">
        {t('codelink-bracket.bracket.loadError')}
      </div>
    );
  }
  if (isLoading || !data) {
    return (
      <div className="grid grid-cols-4 gap-8">
        {Array.from({ length: 4 }, (_, i) => (
          <Skeleton key={i} className="h-72 w-full" />
        ))}
      </div>
    );
  }

  const viewerId = auth.user?.id ?? null;
  const isStaff = (auth.user?.permissions ?? []).includes(CONTEST_MANAGE);

  if (data.matches.length === 0) {
    return isStaff ? (
      <SetupPanel contestId={contestId} />
    ) : (
      <div className="rounded-xl border border-dashed border-border py-16 text-center text-muted-foreground">
        {t('codelink-bracket.bracket.empty')}
      </div>
    );
  }

  const matches = data.matches;
  const mine = matches
    .filter((m) => sideOf(m, viewerId) !== null)
    .sort((a, b) => b.round - a.round)[0];
  const mySide = mine ? sideOf(mine, viewerId) : null;

  return (
    <div className="space-y-6">
      {mine && mySide && (
        <MyMatchStrip contestId={contestId} match={mine} side={mySide} />
      )}
      <div
        ref={presentRef}
        className={cn(
          'space-y-6',
          presenting && 'overflow-auto bg-background p-8',
        )}
      >
        <SummaryBar
          matches={matches}
          isStaff={isStaff}
          presenting={presenting}
          onTogglePresent={() =>
            presenting
              ? void document.exitFullscreen()
              : void presentRef.current?.requestFullscreen()
          }
        />
        <BracketTree
          matches={matches}
          viewerId={presenting ? null : viewerId}
          onOpen={setOpenMatch}
        />
      </div>
      <MatchSheet
        contestId={contestId}
        matchId={openMatch}
        onClose={() => setOpenMatch(null)}
      />
    </div>
  );
}

/** Whether `ref`'s element is currently the fullscreen element. */
function useFullscreen(ref: RefObject<HTMLElement | null>): boolean {
  const [on, setOn] = useState(false);
  useEffect(() => {
    const sync = () => setOn(document.fullscreenElement === ref.current);
    document.addEventListener('fullscreenchange', sync);
    return () => document.removeEventListener('fullscreenchange', sync);
  }, [ref]);
  return on;
}

function SummaryBar({
  matches,
  isStaff,
  presenting,
  onTogglePresent,
}: {
  matches: MatchView[];
  isStaff: boolean;
  presenting: boolean;
  onTogglePresent: () => void;
}) {
  const { t } = useTranslation();
  const decided = matches.filter((m) => m.state === 'decided').length;
  const currentRound =
    Math.min(
      ...matches.filter((m) => m.state !== 'decided').map((m) => m.round),
    ) || ROUND_COUNT;
  const stat = (value: number, label: string, tone?: string) => (
    <div className="flex items-baseline gap-1.5">
      <span
        className={cn('font-mono text-lg font-semibold tabular-nums', tone)}
      >
        {value}
      </span>
      <span className="text-xs text-muted-foreground">{label}</span>
    </div>
  );
  const attention = matches.filter(needsAttention).length;
  return (
    <div className="flex flex-wrap items-center gap-x-8 gap-y-3 rounded-xl border border-border bg-card px-5 py-3">
      <div>
        <div className="text-xs text-muted-foreground">
          {t('codelink-bracket.summary.stage')}
        </div>
        <div className="font-semibold">
          {decided === 15
            ? t('codelink-bracket.summary.finished')
            : t(
                roundNameKey(
                  Number.isFinite(currentRound) ? currentRound : ROUND_COUNT,
                ),
              )}
        </div>
      </div>
      {stat(
        matches.filter(isLive).length,
        t('codelink-bracket.summary.live'),
        'text-emerald-600',
      )}
      {stat(decided, t('codelink-bracket.summary.decided', { total: 15 }))}
      <div className="ml-auto flex items-center gap-3">
        {/* Staff-only signal: kept off the projected view. */}
        {isStaff && !presenting && attention > 0 && (
          <div className="flex items-center gap-2 rounded-lg bg-red-500/10 px-3 py-1.5 text-sm font-medium text-red-700 dark:text-red-300">
            <AlertTriangle className="h-4 w-4" />
            {t('codelink-bracket.summary.attention', { count: attention })}
          </div>
        )}
        <Button size="sm" variant="outline" onClick={onTogglePresent}>
          {presenting ? (
            <Minimize2 className="mr-1.5 h-3.5 w-3.5" />
          ) : (
            <Maximize2 className="mr-1.5 h-3.5 w-3.5" />
          )}
          {presenting
            ? t('codelink-bracket.summary.exitPresent')
            : t('codelink-bracket.summary.present')}
        </Button>
      </div>
    </div>
  );
}

function BracketTree({
  matches,
  viewerId,
  onOpen,
}: {
  matches: MatchView[];
  viewerId: number | null;
  onOpen: (id: number) => void;
}) {
  const { t } = useTranslation();
  const at = new Map(matches.map((m) => [`${m.round}:${m.pos}`, m]));
  const seeds = seedsFrom(matches);
  const final = at.get(`${ROUND_COUNT}:0`);
  const champion =
    final?.state === 'decided' && final.winner !== null
      ? final.winner === final.player_a
        ? playerLabel(final.player_a_name, final.player_a)
        : playerLabel(final.player_b_name, final.player_b)
      : null;

  return (
    <div className="overflow-x-auto pb-2">
      <div className="min-w-[980px]">
        <div className="mb-2 grid grid-cols-[repeat(4,minmax(0,1fr))_9rem] gap-x-8">
          {Array.from({ length: ROUND_COUNT }, (_, i) => (
            <div
              key={i}
              className="text-xs font-semibold uppercase tracking-wide text-muted-foreground"
            >
              {t(roundNameKey(i + 1))}
            </div>
          ))}
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {t('codelink-bracket.round.champion')}
          </div>
        </div>
        <div
          className="grid grid-cols-[repeat(4,minmax(0,1fr))_9rem] gap-x-8"
          style={{ gridTemplateRows: 'repeat(8, minmax(4.75rem, auto))' }}
        >
          {Array.from({ length: ROUND_COUNT }, (_, r) => r + 1).flatMap(
            (round) =>
              Array.from({ length: matchesInRound(round) }, (_, pos) => {
                const span = 1 << (round - 1);
                const match = at.get(`${round}:${pos}`);
                return (
                  <div
                    key={`${round}:${pos}`}
                    className="relative flex items-center py-1.5"
                    style={{
                      gridColumn: round,
                      gridRow: `${pos * span + 1} / span ${span}`,
                    }}
                  >
                    {round > 1 && (
                      <span className="absolute -left-4 top-1/2 h-px w-4 bg-border" />
                    )}
                    <span className="absolute -right-4 top-1/2 h-px w-4 bg-border" />
                    {round < ROUND_COUNT && pos % 2 === 0 && (
                      <span className="absolute -right-4 top-1/2 h-full w-px bg-border" />
                    )}
                    {match ? (
                      <MatchCard
                        match={match}
                        seeds={seeds}
                        isMine={sideOf(match, viewerId) !== null}
                        onOpen={() => onOpen(match.id)}
                      />
                    ) : (
                      <PendingSlot round={round} pos={pos} />
                    )}
                  </div>
                );
              }),
          )}
          <div
            className="relative flex items-center py-1.5"
            style={{ gridColumn: 5, gridRow: '1 / span 8' }}
          >
            <span className="absolute -left-4 top-1/2 h-px w-4 bg-border" />
            <div
              className={cn(
                'flex w-full flex-col items-center gap-2 rounded-xl border p-4 text-center',
                champion
                  ? 'border-amber-400/60 bg-amber-400/10'
                  : 'border-dashed border-border text-muted-foreground',
              )}
            >
              <Crown
                className={cn(
                  'h-6 w-6',
                  champion ? 'text-amber-500' : 'opacity-40',
                )}
              />
              <span className="text-sm font-semibold">
                {champion ?? t('codelink-bracket.round.tbd')}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function PendingSlot({ round, pos }: { round: number; pos: number }) {
  const { t } = useTranslation();
  // Feeders are the two matches of the previous round at 2*pos and 2*pos+1.
  const feeder = (i: number) =>
    t('codelink-bracket.slot.winnerOf', {
      match: `${t(roundShortKey(round - 1))} ${2 * pos + i + 1}`,
    });
  return (
    <div className="w-full rounded-lg border border-dashed border-border px-3 py-2 text-xs text-muted-foreground">
      <div className="py-0.5">
        {round > 1 ? feeder(0) : t('codelink-bracket.round.tbd')}
      </div>
      <div className="py-0.5">
        {round > 1 ? feeder(1) : t('codelink-bracket.round.tbd')}
      </div>
    </div>
  );
}

const PIP: Record<Pip, string> = {
  won: 'bg-emerald-500',
  lost: 'bg-muted-foreground/35',
  void: 'border border-muted-foreground/40',
  live: 'bg-emerald-500 animate-pulse ring-2 ring-emerald-500/30',
  upcoming: 'border border-dashed border-muted-foreground/30',
};

function Pips({ pips }: { pips: Pip[] }) {
  return (
    <span aria-hidden className="flex shrink-0 items-center gap-1">
      {pips.map((p, i) => (
        <span
          key={i}
          className={cn('h-2 w-2 rounded-full', PIP[p], i === 3 && 'ml-1')}
        />
      ))}
    </span>
  );
}

function MatchCard({
  match,
  seeds,
  isMine,
  onOpen,
}: {
  match: MatchView;
  seeds: ReadonlyMap<number, number>;
  isMine: boolean;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const clock = useGameClock(match);
  const startsIn = useStartsIn(match.starts_at_ms);
  const game = match.current_xiaoju_index;
  const liveTag =
    clock && game !== null
      ? game >= 3
        ? t('codelink-bracket.game.shortTiebreak', { n: game - 2 })
        : t('codelink-bracket.game.short', { n: game + 1 })
      : null;
  const row = (name: string, id: number, score: number) => {
    const won = match.winner === id;
    const lost = match.winner !== null && !won;
    return (
      <div
        className={cn(
          'flex items-center gap-2 px-3 py-1',
          won && 'bg-emerald-500/5',
        )}
      >
        <span className="w-4 shrink-0 text-right font-mono text-[10px] text-muted-foreground">
          {seeds.get(id)}
        </span>
        <span
          className={cn(
            'min-w-0 flex-1 truncate text-sm',
            won
              ? 'font-semibold'
              : lost
                ? 'text-muted-foreground'
                : 'font-medium',
          )}
        >
          {name}
        </span>
        {won && <Trophy className="h-3.5 w-3.5 shrink-0 text-amber-500" />}
        {match.state !== 'pending' && match.state !== 'ordering' && (
          <Pips pips={gamePips(match, id)} />
        )}
        <span
          className={cn(
            'w-5 text-right font-mono text-sm tabular-nums',
            won ? 'font-bold' : 'text-muted-foreground',
          )}
        >
          {match.state === 'pending' ? '' : score}
        </span>
      </div>
    );
  };
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        'w-full overflow-hidden rounded-lg border bg-card text-left shadow-xs transition hover:-translate-y-px hover:border-primary/50 hover:shadow-md focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring',
        match.state === 'needs_adjudication'
          ? 'border-red-500/50 ring-1 ring-red-500/30'
          : match.state === 'awaiting_judge'
            ? 'border-amber-500/50 ring-1 ring-amber-500/30'
            : isMine
              ? 'border-primary/60 ring-1 ring-primary/30'
              : 'border-border',
      )}
    >
      <div className="divide-y divide-border">
        {row(
          playerLabel(match.player_a_name, match.player_a),
          match.player_a,
          match.score_a,
        )}
        {row(
          playerLabel(match.player_b_name, match.player_b),
          match.player_b,
          match.score_b,
        )}
      </div>
      <div className="flex items-center gap-2 border-t border-border bg-muted/30 px-3 py-1">
        <span className="font-mono text-[10px] text-muted-foreground">
          {t(roundShortKey(match.round))} {match.pos + 1}
        </span>
        <StatusPill short state={match.state} className="px-1.5 text-[10px]" />
        {isMine && (
          <span className="rounded bg-primary px-1 text-[10px] font-semibold text-primary-foreground">
            {t('codelink-bracket.card.you')}
          </span>
        )}
        {startsIn && (
          <span className="ml-auto font-mono text-[11px] tabular-nums text-sky-700 dark:text-sky-300">
            {startsIn === 'now'
              ? t('codelink-bracket.card.starting')
              : t('codelink-bracket.card.startsIn', { time: startsIn })}
          </span>
        )}
        {clock && (
          <span
            className={cn(
              'ml-auto font-mono text-[11px] tabular-nums',
              clock.overdue ? 'text-amber-700' : 'text-muted-foreground',
            )}
          >
            {liveTag && (
              <span className="mr-1.5 font-sans font-semibold">{liveTag}</span>
            )}
            {clock.text}
          </span>
        )}
      </div>
    </button>
  );
}
