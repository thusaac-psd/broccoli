import { pluginName, useTranslation } from '@broccoli/web-sdk/i18n';
import { closedByText, type PluginOutage } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { PlugZap } from 'lucide-react';

import type { SubmitClosure } from '@/features/problem/utils/submit-closure';

function Notice({ text, className }: { text: string; className?: string }) {
  return (
    <div
      role="status"
      className={cn(
        'flex items-start gap-3 rounded-lg border border-dashed p-4 text-sm text-muted-foreground',
        className,
      )}
    >
      <PlugZap className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
      <p>{text}</p>
    </div>
  );
}

/**
 * What a contest shows while the plugin that provides its contest type
 * cannot serve it (`plugin_outage`): its results are unavailable and its
 * submissions refused. A disabled plugin, or one loaded on no server, is
 * named (`pluginName`), so whoever can bring it back knows which. Renders
 * nothing otherwise.
 */
export function PluginOutageNotice({
  outage,
  className,
}: {
  outage: PluginOutage | null | undefined;
  className?: string;
}) {
  const { t, lookup } = useTranslation();
  if (!outage) return null;
  const text = (() => {
    switch (outage.state) {
      case 'disabled':
        return t('contests.pluginOutage.disabled', {
          plugin: pluginName(lookup, outage.plugin_id),
        });
      case 'not_loaded':
        return t('contests.pluginOutage.notLoaded', {
          plugin: pluginName(lookup, outage.plugin_id),
        });
      case 'unavailable':
        return t('contests.pluginOutage.unavailable');
    }
  })();
  return <Notice text={text} className={className} />;
}

/**
 * What a problem view shows while it takes no submission (`submitClosure`):
 * one notice, naming what closes it. Renders nothing otherwise.
 */
export function SubmitClosureNotice({
  closure,
  className,
}: {
  closure: SubmitClosure | null;
  className?: string;
}) {
  const i18n = useTranslation();
  const { t } = i18n;
  if (!closure) return null;
  switch (closure.kind) {
    case 'contest':
      return (
        <PluginOutageNotice outage={closure.outage} className={className} />
      );
    case 'closed':
      return (
        <Notice
          text={closedByText(closure.closedBy, 'closed', i18n)}
          className={className}
        />
      );
    case 'unavailable':
      return (
        <Notice
          text={t('problem.pluginOutage.unavailable')}
          className={className}
        />
      );
    case 'noLanguage':
      return <Notice text={t('problem.noLanguage')} className={className} />;
  }
}
