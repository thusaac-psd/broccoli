import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';

/**
 * Rules card on the contest overview, the afternoon counterpart of the
 * qualifier's `CodelinkContestInfo`. The rules are fixed by the format, so
 * unlike the qualifier there is no per-contest configuration to fetch.
 */
export function BracketContestInfo({ contestId }: { contestId?: number }) {
  const { t } = useTranslation();
  if (!contestId) return null;
  return (
    <section className="mb-4 rounded-lg border border-border bg-card p-4 text-left">
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
  );
}
