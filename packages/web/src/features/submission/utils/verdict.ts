import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import {
  getStatusLabel,
  type SubmissionStatus,
  type Verdict,
} from '@broccoli/web-sdk/submission';
import type { BadgeProps } from '@broccoli/web-sdk/ui';

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

export function getVerdictBadge(
  verdict: Verdict | null | undefined,
  status: SubmissionStatus,
  t: I18nContextValue['t'],
): {
  label: string;
  variant: BadgeVariant;
} {
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
