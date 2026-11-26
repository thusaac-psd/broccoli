import { pluginName, useTranslation } from '@broccoli/web-sdk/i18n';
import { closedByText, type PluginOutage } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { PlugZap } from 'lucide-react';

import {
  OUTAGE_TEXT,
  type SubmitClosure,
} from '@/features/problem/utils/submit-closure';

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
 * submissions refused. In practice (`scope`), what a problem shows while its
 * practice contest type cannot: its submissions refused. A disabled plugin,
 * one loaded on no server, or one this server lacks while another serves it,
 * is named (`pluginName`), so whoever can bring it back knows which.
 * Renders nothing otherwise.
 */
export function PluginOutageNotice({
  outage,
  scope = 'contest',
  className,
}: {
  outage: PluginOutage | null | undefined;
  scope?: 'contest' | 'practice';
  className?: string;
}) {
  const { t, lookup } = useTranslation();
  if (!outage) return null;
  const key = OUTAGE_TEXT[scope][outage.state];
  const text =
    outage.state === 'unavailable'
      ? t(key)
      : t(key, { plugin: pluginName(lookup, outage.plugin_id) });
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
    case 'practice':
      return (
        <PluginOutageNotice
          outage={closure.outage}
          scope="practice"
          className={className}
        />
      );
    case 'noLanguage':
      return <Notice text={t('problem.noLanguage')} className={className} />;
  }
}
