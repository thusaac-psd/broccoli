import { getErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { type QueryKey, useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';

/**
 * Fetch one record and render `children(record)` once it has arrived.
 *
 * The record is always fetched afresh when the loader mounts, and children
 * only render with a copy fetched since then: an edit form built from it
 * starts from the current record, never from a cached copy or create-mode
 * defaults. A failed fetch shows its error with a retry button instead.
 *
 * Mount one loader per record (`key={id}`), so a late response for another
 * record can never reach the form.
 */
export function RecordLoader<T>({
  queryKey,
  queryFn,
  children,
}: {
  queryKey: QueryKey;
  queryFn: () => Promise<T>;
  children: (record: T) => ReactNode;
}) {
  const { t } = useTranslation();
  const query = useQuery({ queryKey, queryFn, refetchOnMount: 'always' });

  if (query.isError && !query.isFetching) {
    return (
      <div role="alert" className="space-y-3 py-8 text-center">
        <p className="text-sm text-destructive">
          {getErrorMessage(query.error, t('common.loadFailed'))}
        </p>
        <Button variant="outline" size="sm" onClick={() => query.refetch()}>
          {t('common.retry')}
        </Button>
      </div>
    );
  }

  if (!query.isSuccess || !query.isFetchedAfterMount) {
    return (
      <div className="py-8 text-center text-muted-foreground">
        {t('common.loading')}
      </div>
    );
  }

  return children(query.data);
}
