import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { PluginOutage } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { PlugZap } from 'lucide-react';

import type { SubmitClosure } from '@/features/problem/utils/submit-closure';

const CONTEST_MESSAGES = {
  disabled: 'contests.pluginOutage.disabled',
  unavailable: 'contests.pluginOutage.unavailable',
} as const;

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
 * submissions refused. Renders nothing otherwise.
 */
export function PluginOutageNotice({
  outage,
  className,
}: {
  outage: PluginOutage | null | undefined;
  className?: string;
}) {
  const { t } = useTranslation();
  if (!outage) return null;
  return (
    <Notice text={t(CONTEST_MESSAGES[outage.state])} className={className} />
  );
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
  const { t } = useTranslation();
  if (!closure) return null;
  switch (closure.kind) {
    case 'contest':
      return (
        <PluginOutageNotice outage={closure.outage} className={className} />
      );
    case 'closed':
      return (
        <Notice
          text={t(`problem.closedBy.${closure.closedBy.provides}`)}
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
  }
}
