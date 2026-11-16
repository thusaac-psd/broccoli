import type { I18nContextValue } from '@/i18n';
import type {
  PluginOutage,
  ShownStatus,
  Verdict,
  Withheld,
} from '@/submission/types';
import { getStatusLabel, withheldLabel } from '@/submission/utils';
import type { BadgeProps } from '@/ui/badge';

type BadgeVariant = NonNullable<BadgeProps['variant']>;

function getVerdictLabel(verdict: Verdict, t: I18nContextValue['t']) {
  const verdictText = verdictToString(verdict);
  switch (verdictText) {
    case 'Accepted':
      return t('result.accepted');
    case 'WrongAnswer':
      return t('result.wrongAnswer');
    case 'TimeLimitExceeded':
      return t('result.timeLimit');
    case 'MemoryLimitExceeded':
      return t('result.memoryLimit');
    case 'RuntimeError':
      return t('result.runtimeError');
    case 'SystemError':
      return t('result.systemError');
    case 'Skipped':
      return t('result.skipped');
    case 'Cancelled':
      return t('result.cancelled');
    default:
      return verdictText;
  }
}

function getVerdictVariant(verdict: Verdict): BadgeVariant {
  const verdictText = verdictToString(verdict);
  switch (verdictText) {
    case 'Accepted':
      return 'accepted';
    case 'WrongAnswer':
      return 'wronganswer';
    case 'TimeLimitExceeded':
      return 'timelimitexceeded';
    case 'MemoryLimitExceeded':
      return 'memorylimitexceeded';
    case 'RuntimeError':
      return 'runtimeerror';
    case 'SystemError':
    case 'Skipped':
    case 'Cancelled':
      return 'secondary';
    default:
      return 'outline';
  }
}

/** What decides the badge of a submission's outcome, as its responses carry it. */
export interface VerdictOutcome {
  verdict?: Verdict | null;
  status?: ShownStatus;
  withheld?: Withheld | null;
  /** Set while the submission waits for its contest type's provider. */
  plugin_outage?: PluginOutage | null;
}

/**
 * The badge of a submission's outcome. A withheld outcome (`status` `null`)
 * says why it is withheld (`withheld`), never that it is still running; a
 * submission waiting for its contest's plugin says so, never just "Queued".
 */
export function getVerdictBadge(
  { verdict, status, withheld, plugin_outage }: VerdictOutcome,
  t: I18nContextValue['t'],
): {
  label: string;
  variant: BadgeVariant;
} {
  if (status == null) {
    return { label: withheldLabel(withheld, t), variant: 'outline' };
  }
  if (plugin_outage) {
    return {
      label:
        plugin_outage.state === 'disabled'
          ? t('submissions.waitingForPlugin.disabled')
          : t('submissions.waitingForPlugin.unavailable'),
      variant: 'outline',
    };
  }
  if (status === 'Pending' || status === 'Compiling' || status === 'Running') {
    return { label: getStatusLabel(status, t), variant: 'outline' };
  }
  if (status === 'CompilationError') {
    return { label: t('result.compilationError'), variant: 'secondary' };
  }
  if (status === 'SystemError') {
    return { label: t('result.systemError'), variant: 'secondary' };
  }
  if (!verdict) {
    return { label: getStatusLabel(status, t), variant: 'outline' };
  }
  return {
    label: getVerdictLabel(verdict, t),
    variant: getVerdictVariant(verdict),
  };
}

export function verdictToString(verdict: Verdict): string;
export function verdictToString(
  verdict: Verdict | null | undefined,
): string | null;
export function verdictToString(
  verdict: Verdict | null | undefined,
): string | null {
  if (!verdict) return null;
  // Verdict is always serialized as a plain string by the server
  // (including Other variants like custom plugin verdicts).
  return String(verdict);
}
