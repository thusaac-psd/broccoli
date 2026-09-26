import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button } from '@broccoli/web-sdk/ui';

import type { ContestInfoResponse } from './types';
import { useCodelinkQuery } from './useCodelinkQuery';

export function CodelinkContestInfo({ contestId }: { contestId?: number }) {
  const { t } = useTranslation();
  const { data, isError, isPending, refetch } =
    useCodelinkQuery<ContestInfoResponse>(contestId, 'info');
  if (!contestId) return null;
  if (isPending)
    return (
      <p className="mb-4 text-sm text-muted-foreground">
        {t('codelink-qualifier.loading')}
      </p>
    );
  if (!data) {
    return (
      <div role="alert" className="mb-4 rounded-lg border border-border p-4">
        <p>{t('codelink-qualifier.loadError')}</p>
        <Button
          className="mt-2"
          variant="outline"
          onClick={() => void refetch()}
        >
          {t('codelink-qualifier.refresh')}
        </Button>
      </div>
    );
  }
  return (
    <section className="mb-4 rounded-lg border border-border bg-card p-4 text-left">
      <div className="mb-2 flex items-center gap-2">
        <Badge>Codelink</Badge>
        <h2 className="font-semibold">{t('codelink-qualifier.rules.title')}</h2>
      </div>
      <p className="text-sm text-muted-foreground">
        {t('codelink-qualifier.rules.description', {
          problem_count: data.problem_count,
          slots_per_problem: data.slots_per_problem,
          solves_to_qualify: data.solves_to_qualify,
        })}
      </p>
      {isError && (
        <p role="alert" className="mt-2 text-sm text-destructive">
          {t('codelink-qualifier.stale')}
        </p>
      )}
      <p className="mt-2 text-sm text-muted-foreground">
        {t('codelink-qualifier.rules.order')}
      </p>
    </section>
  );
}
