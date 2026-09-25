import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { useState } from 'react';

import { useMyMatch } from './hooks/useMyMatch';
import { MatchSheet } from './MatchSheet';
import { MyMatchPanel } from './MyMatchPanel';

/**
 * The contest homepage for the afternoon round. Contestants land here, so it
 * leads with their own match (rank the opponent's problems, then the
 * current problem and clock), followed by the rules card - the counterpart
 * of the qualifier's `CodelinkContestInfo`. The rules are fixed by the
 * format, so there is no per-contest configuration to fetch.
 */
export function BracketContestInfo({ contestId }: { contestId?: number }) {
  const { t } = useTranslation();
  const mine = useMyMatch(contestId);
  const [openMatch, setOpenMatch] = useState<number | null>(null);
  if (!contestId) return null;
  return (
    <div className="mb-4 space-y-4">
      {mine && (
        <MyMatchPanel
          contestId={contestId}
          match={mine.match}
          side={mine.side}
          onOpenDetails={() => setOpenMatch(mine.match.id)}
        />
      )}
      <section className="rounded-lg border border-border bg-card p-4 text-left">
        <div className="mb-2 flex items-center gap-2">
          <Badge>Codelink</Badge>
          <h2 className="font-semibold">{t('codelink-bracket.rules.title')}</h2>
        </div>
        <p className="text-sm text-muted-foreground">
          {t('codelink-bracket.rules.description')}
        </p>
        <p className="mt-2 text-sm text-muted-foreground">
          {t('codelink-bracket.rules.order')}
        </p>
      </section>
      <MatchSheet
        contestId={contestId}
        matchId={openMatch}
        onClose={() => setOpenMatch(null)}
      />
    </div>
  );
}
