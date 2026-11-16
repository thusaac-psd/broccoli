import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { PluginOutage } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { PlugZap } from 'lucide-react';

const MESSAGES = {
  contest: {
    disabled: 'contests.pluginOutage.disabled',
    unavailable: 'contests.pluginOutage.unavailable',
  },
  problem: {
    disabled: 'problem.pluginOutage.disabled',
    unavailable: 'problem.pluginOutage.unavailable',
  },
} as const;

/**
 * What a contest, or a problem's practice submit, shows while the plugin
 * that provides its contest type cannot serve it (`plugin_outage`): a
 * contest's results are unavailable and its submissions refused, a
 * problem's practice submissions refused. Renders nothing otherwise.
 */
export function PluginOutageNotice({
  outage,
  of,
  className,
}: {
  outage: PluginOutage | null | undefined;
  of: keyof typeof MESSAGES;
  className?: string;
}) {
  const { t } = useTranslation();
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
      <p>{t(MESSAGES[of][outage.state])}</p>
    </div>
  );
}
