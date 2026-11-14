import { useTranslation } from '@broccoli/web-sdk/i18n';
import { QueryLoading, QueryView } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { Printer } from 'lucide-react';

import { usePrintApi } from '../hooks/usePrintApi';
import type { PrintJob } from '../types';
import { StatusPill } from './StatusPill';

interface Props {
  contestId?: number;
}

export function MyPrintsPanel({ contestId }: Props) {
  const { t } = useTranslation();
  const api = usePrintApi();

  const query = useQuery({
    queryKey: ['print', 'my-jobs', contestId],
    queryFn: () => api.myJobs(contestId),
    refetchInterval: 10_000,
    enabled: !!contestId,
  });

  const jobs: PrintJob[] = query.data?.data ?? [];

  return (
    <div className="rounded-lg border border-border bg-card">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <Printer className="h-3.5 w-3.5 text-muted-foreground" />
        <span className="text-xs font-medium text-foreground">
          {t('print.myPrints.title')}
        </span>
        {jobs.length > 0 && (
          <span className="ml-auto text-xs text-muted-foreground tabular-nums">
            {jobs.length}
          </span>
        )}
      </div>
      <QueryView
        query={query}
        idle={
          <p className="px-3 py-2 text-xs text-muted-foreground">
            {t('print.myPrints.noContest')}
          </p>
        }
        loading={<QueryLoading className="p-3" />}
        failure={t('print.myPrints.loadFailed')}
        isEmpty={(resp) => resp.data.length === 0}
        empty={
          <p className="px-3 py-2 text-xs text-muted-foreground">
            {t('print.myPrints.empty')}
          </p>
        }
      >
        {(resp) => (
          <div className="divide-y divide-border">
            {resp.data.slice(0, 5).map((job) => (
              <div
                key={job.id}
                className="flex items-center gap-2 px-3 py-1.5 text-xs"
              >
                <span className="w-8 shrink-0 text-muted-foreground tabular-nums">
                  #{job.id}
                </span>
                <span className="min-w-0 truncate text-foreground">
                  {job.filename}
                </span>
                <StatusPill status={job.status} />
              </div>
            ))}
          </div>
        )}
      </QueryView>
    </div>
  );
}
