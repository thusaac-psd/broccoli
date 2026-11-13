import { getErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { type QueryKey, useQuery } from '@tanstack/react-query';
import { type ReactNode, useState } from 'react';

/**
 * Fetch one record and render `children(record)` once it has arrived.
 *
 * By default the record is fetched afresh when the loader mounts, and
 * children only render with a copy fetched since then: an edit form built
 * from it starts from the current record, never from a cached copy or
 * create-mode defaults. That first copy is then kept for the loader's
 * lifetime: a later refetch, failed or not, never replaces a mounted form,
 * and the loader does not refetch on window focus or reconnect. Until the
 * record arrives a failed fetch shows its error with a retry button.
 *
 * `allowCached` renders a cached copy at once instead, for read-only views.
 *
 * Mount one loader per record (`key={id}`), so a late response for another
 * record can never reach the form.
 */
export function RecordLoader<T>({
  queryKey,
  queryFn,
  allowCached = false,
  children,
}: {
  queryKey: QueryKey;
  queryFn: () => Promise<T>;
  allowCached?: boolean;
  children: (record: T) => ReactNode;
}) {
  const { t } = useTranslation();
  const query = useQuery({
    queryKey,
    queryFn,
    refetchOnMount: allowCached ? true : 'always',
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const [loaded, setLoaded] = useState<{ record: T } | null>(null);

  const usable = query.isSuccess && (allowCached || query.isFetchedAfterMount);
  if (loaded === null && usable) {
    // The first usable copy; set during render so children mount with it.
    setLoaded({ record: query.data });
  }

  if (loaded !== null) return children(loaded.record);
  if (usable) return children(query.data);

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

  return (
    <div className="py-8 text-center text-muted-foreground">
      {t('common.loading')}
    </div>
  );
}
