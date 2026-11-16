import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Card, CardContent } from '@broccoli/web-sdk/ui';
import { CheckCircle2, Clock, Inbox } from 'lucide-react';

import type { DlqStats } from '@/features/dlq/types';

interface Props {
  stats: DlqStats;
}

export function DlqStatsSummary({ stats }: Props) {
  const { t } = useTranslation();

  const items = [
    {
      labelKey: 'dlq.stats.unresolved',
      value: stats.total_unresolved,
      icon: Inbox,
      tone:
        stats.total_unresolved > 0 ? ('alert' as const) : ('default' as const),
    },
    {
      labelKey: 'dlq.stats.resolved',
      value: stats.total_resolved,
      icon: CheckCircle2,
      tone: 'default' as const,
    },
    {
      labelKey: 'dlq.stats.operationTask',
      value: stats.unresolved_by_message_type.operation_task,
      icon: Clock,
      tone: 'default' as const,
    },
    {
      labelKey: 'dlq.stats.stuckSubmission',
      value: stats.unresolved_by_message_type.stuck_submission,
      icon: Clock,
      tone: 'default' as const,
    },
    {
      labelKey: 'dlq.stats.stuckCodeRun',
      value: stats.unresolved_by_message_type.stuck_code_run,
      icon: Clock,
      tone: 'default' as const,
    },
    {
      labelKey: 'dlq.stats.stuckSubmissionJudgement',
      value: stats.unresolved_by_message_type.stuck_submission_judgement,
      icon: Clock,
      tone: 'default' as const,
    },
  ] as const;

  return (
    <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6">
      {items.map(({ labelKey, value, icon: Icon, tone }) => (
        <Card key={labelKey}>
          <CardContent className="pt-6">
            <div className="flex items-start justify-between gap-3">
              <div>
                <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  {t(labelKey)}
                </p>
                <p
                  className={`mt-2 text-3xl font-semibold tabular-nums ${
                    tone === 'alert' ? 'text-destructive' : 'text-foreground'
                  }`}
                >
                  {value}
                </p>
              </div>
              <Icon
                className={`h-5 w-5 shrink-0 ${
                  tone === 'alert'
                    ? 'text-destructive'
                    : 'text-muted-foreground'
                }`}
              />
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}
