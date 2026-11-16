import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Card, CardContent } from '@broccoli/web-sdk/ui';
import { Activity, Inbox, Server, Zap } from 'lucide-react';

interface Props {
  workersOnline: number;
  totalInFlight: number;
  submissionsInProgress: number;
  dlqUnresolved: number;
}

export function SystemHealthSummary({
  workersOnline,
  totalInFlight,
  submissionsInProgress,
  dlqUnresolved,
}: Props) {
  const { t } = useTranslation();

  const items = [
    {
      labelKey: 'system.summary.workers',
      value: workersOnline,
      icon: Server,
      tone: 'default' as const,
    },
    {
      labelKey: 'system.summary.inFlight',
      value: totalInFlight,
      icon: Zap,
      tone: 'default' as const,
    },
    {
      labelKey: 'system.summary.submissionsInProgress',
      value: submissionsInProgress,
      icon: Activity,
      tone: 'default' as const,
    },
    {
      labelKey: 'system.summary.dlqUnresolved',
      value: dlqUnresolved,
      icon: Inbox,
      tone: dlqUnresolved > 0 ? ('alert' as const) : ('default' as const),
    },
  ] as const;

  return (
    <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
      {items.map(({ labelKey, value, icon: Icon, tone }) => (
        <Card key={labelKey}>
          <CardContent className="pt-6">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
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
