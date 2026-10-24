import { useTranslation } from '@broccoli/web-sdk/i18n';
import { cn } from '@broccoli/web-sdk/utils';

import { gamePips, type Pip } from './lib/pips';
import { playerLabel } from './lib/player';
import { roundShortKey } from './lib/rounds';
import { gameLabel } from './lib/stage';
import { StatusPill, useGameClock, useStartsIn } from './parts';
import type { MatchView } from './types';

const PIP: Record<Pip, string> = {
  won: 'bg-emerald-500',
  lost: 'bg-red-500/70',
  void: 'border border-muted-foreground/40',
  live: 'bg-primary',
  upcoming: 'border border-dashed border-muted-foreground/30',
};

const PIP_LABEL: Record<Pip, string> = {
  won: 'codelink-bracket.game.won',
  lost: 'codelink-bracket.game.lost',
  void: 'codelink-bracket.game.noScore',
  live: 'codelink-bracket.game.live',
  upcoming: 'codelink-bracket.game.upcoming',
};

function Pips({ match, playerId }: { match: MatchView; playerId: number }) {
  const { t } = useTranslation();
  const pips = gamePips(match, playerId);
  const labels = pips.map(
    (pip, index) =>
      `${t(...gameLabel(match, index, true))} · ${t(PIP_LABEL[pip])}`,
  );
  return (
    <span
      role="img"
      aria-label={labels.join('; ')}
      className="flex min-w-0 items-center gap-1 overflow-x-auto"
    >
      {pips.map((p, i) => (
        <span
          key={i}
          title={labels[i]}
          className={cn(
            'h-2.5 w-2.5 shrink-0 rounded-full',
            PIP[p],
            i === match.group_a.length && 'ml-2',
          )}
        />
      ))}
    </span>
  );
}

export function BracketMatchCard({
  match,
  seeds,
  isMine,
  onOpen,
  horizontal = false,
}: {
  match: MatchView;
  seeds: ReadonlyMap<number, number>;
  isMine: boolean;
  onOpen: () => void;
  horizontal?: boolean;
}) {
  const { t } = useTranslation();
  const clock = useGameClock(match);
  const startsIn = useStartsIn(
    match.state === 'intermission' ? match.next_game_at_ms : match.starts_at_ms,
  );
  const game = match.current_xiaoju_index;
  const liveTag =
    clock && game !== null ? t(...gameLabel(match, game, true)) : null;
  const row = (name: string, id: number, score: number) => {
    const won = match.winner === id;
    const lost = match.winner !== null && !won;
    return (
      <div
        className={cn(
          'grid min-w-0 grid-cols-[1rem_minmax(0,1fr)_1.25rem] items-center gap-x-2 px-3 py-1.5',
          won && 'bg-muted/40',
        )}
      >
        <span className="w-4 shrink-0 text-right font-mono text-[10px] text-muted-foreground">
          {seeds.get(id)}
        </span>
        <span
          title={name}
          className={cn(
            'min-w-0 truncate text-sm',
            won
              ? 'font-semibold'
              : lost
                ? 'text-muted-foreground'
                : 'font-medium',
          )}
        >
          {name}
        </span>
        <span
          className={cn(
            'w-5 text-right font-mono text-sm tabular-nums',
            won ? 'font-bold' : 'text-muted-foreground',
          )}
        >
          {match.state === 'pending' ? '' : score}
        </span>
        {match.state !== 'pending' && match.state !== 'ordering' && (
          <div className="col-start-2 col-span-2 min-w-0 pt-1">
            <Pips match={match} playerId={id} />
          </div>
        )}
      </div>
    );
  };
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        'flex h-full w-full flex-col overflow-hidden rounded-lg border bg-background text-left transition-colors hover:bg-muted/50 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring',
        match.state === 'needs_adjudication'
          ? 'border-red-500/50'
          : match.state === 'awaiting_judge'
            ? 'border-amber-500/50'
            : isMine
              ? 'border-primary/60'
              : 'border-border',
      )}
    >
      <div
        className={cn(
          'flex-1',
          horizontal
            ? 'grid grid-cols-2 divide-x divide-border'
            : 'flex flex-col justify-center divide-y divide-border',
        )}
      >
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
      <div className="flex items-center gap-2 border-t border-border bg-muted/50 px-3 py-1.5">
        <span className="font-mono text-[10px] text-muted-foreground">
          {t(
            match.kind === 'third_place'
              ? 'codelink-bracket.round.short.thirdPlace'
              : roundShortKey(match.round, match.stage_count),
          )}
          {match.kind === 'elimination' ? ` ${match.pos + 1}` : ''}
        </span>
        <StatusPill
          short
          state={match.state}
          className="border-0 bg-transparent px-0 py-0 text-xs"
        />
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
