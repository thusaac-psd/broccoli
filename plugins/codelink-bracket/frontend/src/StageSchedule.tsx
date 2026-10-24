import { useTranslation } from '@broccoli/web-sdk/i18n';
import { ChevronDown } from 'lucide-react';

import { roundNameKey } from './lib/rounds';
import type { StageView } from './types';

export function StageSchedule({
  stages,
  thirdPlace,
}: {
  stages: StageView[];
  thirdPlace: boolean;
}) {
  const { t } = useTranslation();
  const date = (at: number | null) =>
    at === null
      ? t('codelink-bracket.schedule.unset')
      : new Intl.DateTimeFormat(undefined, {
          month: 'short',
          day: 'numeric',
          hour: '2-digit',
          minute: '2-digit',
        }).format(at);
  return (
    <details className="group rounded-lg border border-border">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-medium hover:bg-muted/50">
        <span>{t('codelink-bracket.schedule.title')}</span>
        <span className="ml-auto text-xs font-normal text-muted-foreground">
          {Intl.DateTimeFormat().resolvedOptions().timeZone}
        </span>
        <ChevronDown className="h-4 w-4 text-muted-foreground transition-transform group-open:rotate-180" />
      </summary>
      <div className="overflow-x-auto border-t border-border">
        <table className="w-full text-sm">
          <thead className="bg-muted/50 text-xs text-muted-foreground">
            <tr>
              <th className="px-4 py-2 text-left font-medium">
                {t('codelink-bracket.summary.stage')}
              </th>
              <th className="px-4 py-2 text-left font-medium">
                {t('codelink-bracket.schedule.orderingAt')}
              </th>
              <th className="px-4 py-2 text-left font-medium">
                {t('codelink-bracket.schedule.playAt')}
              </th>
              <th className="px-4 py-2 text-left font-medium">
                {t('codelink-bracket.schedule.status')}
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {stages.map((stage) => (
              <tr key={stage.round}>
                <td className="whitespace-nowrap px-4 py-3 font-medium">
                  {t(
                    stage.round === stages.length && thirdPlace
                      ? 'codelink-bracket.round.finals'
                      : roundNameKey(stage.round, stages.length),
                  )}
                </td>
                <td className="whitespace-nowrap px-4 py-3 tabular-nums text-muted-foreground">
                  {date(stage.start_at_ms)}
                </td>
                <td className="whitespace-nowrap px-4 py-3 tabular-nums text-muted-foreground">
                  {date(stage.starts_at_ms)}
                </td>
                <td className="px-4 py-3">
                  <span
                    className={
                      stage.phase === 'awaiting_reschedule'
                        ? 'text-amber-700 dark:text-amber-300'
                        : 'text-muted-foreground'
                    }
                  >
                    {t(`codelink-bracket.schedule.${stage.phase}`)}
                  </span>
                  {stage.phase === 'awaiting_reschedule' && (
                    <p className="mt-1 text-xs text-muted-foreground">
                      {t('codelink-bracket.schedule.rescheduleReason')}
                    </p>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}
