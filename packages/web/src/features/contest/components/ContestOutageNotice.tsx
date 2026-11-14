import type { Contest } from '@broccoli/web-sdk/contest';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { cn } from '@broccoli/web-sdk/utils';
import { PlugZap } from 'lucide-react';

/**
 * What a contest shows while the plugin that provides its type cannot serve
 * it (`Contest.plugin_outage`): its results are unavailable and its
 * submissions are refused. Renders nothing otherwise.
 */
export function ContestOutageNotice({
  contest,
  className,
}: {
  contest: Pick<Contest, 'plugin_outage'> | undefined;
  className?: string;
}) {
  const { t } = useTranslation();
  const outage = contest?.plugin_outage;
  if (!outage) return null;
  return (
    <div
      role="status"
      className={cn(
        'flex items-start gap-3 rounded-lg border border-dashed p-4 text-sm text-muted-foreground',
        className,
      )}
    >
      <PlugZap className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
      <p>
        {outage.state === 'disabled'
          ? t('contests.pluginOutage.disabled')
          : t('contests.pluginOutage.unavailable')}
      </p>
    </div>
  );
}
