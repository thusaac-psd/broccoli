import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useQuery } from '@tanstack/react-query';

import { useIoiApi } from './hooks/useIoiApi';
import { ioiKeys } from './query-keys';

interface IoiConfigBannerProps {
  scope?: {
    scope?: string;
    contestId?: number;
    contest_id?: number;
  };
}

export function IoiConfigBanner({ scope }: IoiConfigBannerProps) {
  const { t } = useTranslation();
  const api = useIoiApi();

  const contestId = scope?.contestId ?? scope?.contest_id;
  const isContestScope =
    scope?.scope === 'contest' || scope?.scope === 'contest_problem';

  const { data: scoreboard } = useQuery({
    queryKey: ioiKeys.scoreboard(contestId),
    enabled: !!contestId && isContestScope,
    queryFn: () => api.getScoreboard(contestId!),
    staleTime: 60000,
    retry: false,
  });

  if (!isContestScope || !contestId) return null;

  const phase = scoreboard?.phase;

  if (phase === 'during') {
    return (
      <div className="p-2.5 px-3.5 rounded-md mb-3 text-[13px] font-medium bg-amber-500/10 text-amber-700 border border-amber-500/20 flex items-center gap-2">
        <span className="text-base">!</span>
        {t('ioi.config.activeContestWarning')}
      </div>
    );
  }

  if (phase === 'before') {
    return (
      <div className="p-2 px-3.5 rounded-md mb-3 text-xs text-muted-foreground bg-muted border border-border">
        {t('ioi.config.beforeContestInfo')}
      </div>
    );
  }

  return null;
}
