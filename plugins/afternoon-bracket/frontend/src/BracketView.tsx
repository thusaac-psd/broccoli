import { useTranslation } from '@broccoli/web-sdk/i18n';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import { type ReactNode, useState } from 'react';

import { useBracketApi } from './hooks/useBracketApi';
import { describeMatchPhase } from './lib/phase';
import { playerLabel } from './lib/player';
import { MatchDetailPanel } from './MatchDetailPanel';
import type { MatchView } from './types';

interface BracketViewProps {
  contestId?: number;
  contestType?: string;
  children?: ReactNode;
}

/**
 * The bracket view: 16 slots across 4 rounds, grouped by round. Registered
 * at the `ranking.content` slot (contest_type = "afternoon-bracket",
 * position = "wrap", mirroring IcpcScoreboard/IoiScoreboard). Every match
 * shown here is already visibility-filtered by `GET /bracket` for this
 * viewer -- this component does not re-derive or second-guess that, it
 * only renders what came back.
 */
export function BracketView({ contestId, children }: BracketViewProps) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const [selectedMatchId, setSelectedMatchId] = useState<number | null>(null);

  const { data, isLoading, isError } = useQuery({
    queryKey: ['afternoon-bracket-bracket', contestId],
    enabled: !!contestId,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval: 8_000,
  });

  if (!contestId) {
    return <>{children}</>;
  }

  if (isError) {
    return (
      <div className="rounded-md bg-red-500/[0.06] p-6 text-center text-[13px] text-red-600">
        {t('afternoon-bracket.bracket.loadError')}
      </div>
    );
  }

  if (isLoading || !data) {
    return (
      <div className="p-6 text-center text-muted-foreground">
        {t('afternoon-bracket.bracket.loading')}
      </div>
    );
  }

  if (data.matches.length === 0) {
    return (
      <div className="py-12 text-center text-muted-foreground">
        {t('afternoon-bracket.bracket.empty')}
      </div>
    );
  }

  const byRound = new Map<number, MatchView[]>();
  for (const match of data.matches) {
    const bucket = byRound.get(match.round) ?? [];
    bucket.push(match);
    byRound.set(match.round, bucket);
  }
  const rounds = [...byRound.keys()].sort((a, b) => a - b);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-4 overflow-x-auto sm:flex-row">
        {rounds.map((round) => (
          <div key={round} className="flex min-w-[220px] flex-col gap-2">
            <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
              {t('afternoon-bracket.bracket.round', { round })}
            </h3>
            {(byRound.get(round) ?? [])
              .sort((a, b) => a.pos - b.pos)
              .map((match) => (
                <MatchCard
                  key={match.id}
                  match={match}
                  selected={selectedMatchId === match.id}
                  onSelect={() => setSelectedMatchId(match.id)}
                />
              ))}
          </div>
        ))}
      </div>

      {selectedMatchId !== null && (
        <MatchDetailPanel contestId={contestId} matchId={selectedMatchId} />
      )}
    </div>
  );
}

function MatchCard({
  match,
  selected,
  onSelect,
}: {
  match: MatchView;
  selected: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const status = describeMatchPhase(match.state);
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        'rounded-md border border-border p-2 text-left text-sm transition-colors hover:bg-accent',
        selected && 'border-primary bg-accent',
      )}
    >
      <div className="flex items-center justify-between">
        <span
          className={cn(
            'font-medium',
            match.winner === match.player_a && 'text-emerald-600',
          )}
        >
          {playerLabel(match.player_a_name, match.player_a)}
        </span>
        <span className="font-mono tabular-nums">{match.score_a}</span>
      </div>
      <div className="flex items-center justify-between">
        <span
          className={cn(
            'font-medium',
            match.winner === match.player_b && 'text-emerald-600',
          )}
        >
          {playerLabel(match.player_b_name, match.player_b)}
        </span>
        <span className="font-mono tabular-nums">{match.score_b}</span>
      </div>
      <div className="mt-1 text-[11px] text-muted-foreground">
        {t(status.labelKey)}
      </div>
    </button>
  );
}
