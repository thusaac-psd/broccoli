import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { useParams } from 'react-router';

import { useIsIcpcContest } from './hooks/useIsIcpcContest';

export function IcpcContestInfo() {
  const { contestId } = useParams();
  const cId = contestId ? Number(contestId) : undefined;
  const { isIcpc, contestInfo, isLoading } = useIsIcpcContest(cId);
  const { t } = useTranslation();

  if (isLoading || !isIcpc || !contestInfo) return null;

  return (
    <div className="rounded-lg border border-border bg-card mb-4 p-4 text-left">
      <div className="flex items-center gap-2 mb-2">
        <Badge
          variant="default"
          className="uppercase text-[11px] font-bold tracking-wide"
        >
          ICPC
        </Badge>
        <span className="text-sm font-semibold text-foreground">
          {t('icpc.contestInfo.title')}
        </span>
      </div>

      <div className="text-xs text-muted-foreground mb-2.5">
        {t('icpc.contestInfo.description', {
          penalty_minutes: contestInfo.penalty_minutes,
        })}{' '}
        {contestInfo.count_compile_error
          ? t('icpc.contestInfo.ceCount')
          : t('icpc.contestInfo.ceNoCount')}
      </div>

      <div className="flex flex-wrap gap-4 text-xs text-muted-foreground justify-start">
        <span className="inline-flex items-center gap-1 rounded bg-muted text-[11px] font-medium px-1.5 py-0.5">
          {t('icpc.contestInfo.penaltyPerAttempt', {
            penalty_minutes: contestInfo.penalty_minutes,
          })}
        </span>
        {contestInfo.show_test_details && (
          <span className="inline-flex items-center gap-1 rounded bg-muted text-[11px] font-medium px-1.5 py-0.5">
            {t('icpc.contestInfo.testDetailsVisible')}
          </span>
        )}
      </div>
    </div>
  );
}
