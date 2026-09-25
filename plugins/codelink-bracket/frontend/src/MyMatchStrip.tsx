import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { ArrowRight, Swords } from 'lucide-react';
import { Link } from 'react-router';

import { playerLabel } from './lib/player';
import { roundNameKey } from './lib/rounds';
import { playerStage, type Side } from './lib/stage';
import { useGameClock } from './parts';
import type { MatchView } from './types';

/**
 * One line above the bracket pointing a player back to their match. The
 * full panel (ranking, current problem) lives on the contest homepage, where
 * contestants land; the ranking page stays the shared bracket view.
 */
export function MyMatchStrip({
  contestId,
  match,
  side,
}: {
  contestId: number;
  match: MatchView;
  side: Side;
}) {
  const { t } = useTranslation();
  const clock = useGameClock(match);
  const stage = playerStage(match, side);
  const opponent =
    side === 'a'
      ? playerLabel(match.player_b_name, match.player_b)
      : playerLabel(match.player_a_name, match.player_a);
  const status =
    stage.kind === 'play'
      ? `${
          stage.game >= 3
            ? t('codelink-bracket.game.tiebreak', { n: stage.game - 2 })
            : t('codelink-bracket.game.regular', { n: stage.game + 1 })
        }${clock ? ` · ${clock.text}` : ''}`
      : t(`codelink-bracket.strip.${stage.kind}`);
  const urgent = stage.kind === 'rank' || stage.kind === 'play';
  return (
    <div
      className={cn(
        'flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border px-4 py-2.5 text-sm',
        urgent ? 'border-primary/40 bg-primary/5' : 'border-border bg-card',
      )}
    >
      <Swords className="h-4 w-4 text-primary" />
      <span className="font-medium">{t('codelink-bracket.my.title')}</span>
      <span className="text-muted-foreground">
        {t(roundNameKey(match.round))} ·{' '}
        {t('codelink-bracket.my.vs', { name: opponent })}
      </span>
      <span className={cn('font-medium', urgent && 'text-primary')}>
        {status}
      </span>
      <Button
        asChild
        size="sm"
        variant={urgent ? 'default' : 'outline'}
        className="ml-auto"
      >
        <Link to={`/contests/${contestId}`}>
          {t('codelink-bracket.strip.go')}
          <ArrowRight className="ml-1.5 h-3.5 w-3.5" />
        </Link>
      </Button>
    </div>
  );
}
