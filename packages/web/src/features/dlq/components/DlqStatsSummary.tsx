import { type TranslationKey, useTranslation } from '@broccoli/web-sdk/i18n';
import { Card, CardContent } from '@broccoli/web-sdk/ui';
import { CheckCircle2, Clock, Inbox } from 'lucide-react';

import type { DlqStats } from '@/features/dlq/types';

interface Props {
  stats: DlqStats;
}

/**
 * The label of each message type's card. Typed against the API's counts, so
 * a message type the server adds or drops fails to compile here instead of
 * missing from the summary.
 */
const MESSAGE_TYPE_LABELS = {
  operation_task: 'dlq.stats.operationTask',
  stuck_submission: 'dlq.stats.stuckSubmission',
  stuck_code_run: 'dlq.stats.stuckCodeRun',
  stuck_submission_judgement: 'dlq.stats.stuckSubmissionJudgement',
  hook_event: 'dlq.stats.hookEvent',
} as const satisfies Record<
  keyof DlqStats['unresolved_by_message_type'],
  TranslationKey
>;

type MessageType = keyof typeof MESSAGE_TYPE_LABELS;

export function DlqStatsSummary({ stats }: Props) {
  const { t } = useTranslation();

  const items: {
    labelKey: TranslationKey;
    value: number;
    icon: typeof Inbox;
    tone: 'alert' | 'default';
  }[] = [
    {
      labelKey: 'dlq.stats.unresolved',
      value: stats.total_unresolved,
      icon: Inbox,
      tone: stats.total_unresolved > 0 ? 'alert' : 'default',
    },
    {
      labelKey: 'dlq.stats.resolved',
      value: stats.total_resolved,
      icon: CheckCircle2,
      tone: 'default',
    },
    ...(Object.keys(MESSAGE_TYPE_LABELS) as MessageType[]).map((type) => ({
      labelKey: MESSAGE_TYPE_LABELS[type],
      value: stats.unresolved_by_message_type[type],
      icon: Clock,
      tone: 'default' as const,
    })),
  ];

  // Cards wrap at a width that holds their labels whole, and grow to fill
  // their row, so however many there are, no row ends in a lone narrow card.
  return (
    <div data-testid="dlq-stats-summary" className="flex flex-wrap gap-4">
      {items.map(({ labelKey, value, icon: Icon, tone }) => (
        <Card key={labelKey} className="min-w-0 flex-[1_1_13rem]">
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
