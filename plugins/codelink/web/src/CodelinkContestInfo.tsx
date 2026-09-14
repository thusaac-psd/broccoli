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
        {t('codelink.loading')}
      </p>
    );
  if (!data) {
    return (
      <div role="alert" className="mb-4 rounded-lg border border-border p-4">
        <p>{t('codelink.loadError')}</p>
        <Button
          className="mt-2"
          variant="outline"
          onClick={() => void refetch()}
        >
          {t('codelink.refresh')}
        </Button>
      </div>
    );
  }
  return (
    <section className="mb-4 rounded-lg border border-border bg-card p-4 text-left">
      <div className="mb-2 flex items-center gap-2">
        <Badge>Codelink</Badge>
        <h2 className="font-semibold">{t('codelink.rules.title')}</h2>
      </div>
      <p className="text-sm text-muted-foreground">
        {t('codelink.rules.description', {
          problem_count: data.problem_count,
          slots_per_problem: data.slots_per_problem,
          solves_to_qualify: data.solves_to_qualify,
        })}
      </p>
      {isError && (
        <p role="alert" className="mt-2 text-sm text-destructive">
          {t('codelink.stale')}
        </p>
      )}
      <p className="mt-2 text-sm text-muted-foreground">
        {t('codelink.rules.order')}
      </p>
    </section>
  );
}
