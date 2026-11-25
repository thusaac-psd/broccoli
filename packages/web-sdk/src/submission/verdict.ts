import type { I18nContextValue } from '@/i18n';
import type { SubmissionOutcome } from '@/submission/progress';
import type { ShownStatus, Verdict } from '@/submission/types';
import type { BadgeProps } from '@/ui/badge';

import { closedByText } from './closed-by.ts';
import { getStatusLabel, withheldLabel } from './utils.ts';

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
export interface VerdictOutcome extends SubmissionOutcome {
  verdict?: Verdict | null;
}

/**
 * A `VerdictOutcome` copied into a row of a list: every field is required, so
 * a row built from a response cannot leave one out (and show a parked or
 * withheld submission as being judged).
 */
export type RowOutcome = {
  [K in keyof Required<VerdictOutcome>]: VerdictOutcome[K];
};

/**
 * The badge of a submission's outcome. A withheld outcome (`status` `null`)
 * says why it is withheld (`withheld`), never that it is still running; a
 * parked submission names the plugin that parks it and its role
 * (`closedByText`), never just "Queued".
 */
export function getVerdictBadge(
  { verdict, status, withheld, parked_by }: VerdictOutcome,
  i18n: Pick<I18nContextValue, 't' | 'lookup'>,
): {
  label: string;
  variant: BadgeVariant;
} {
  const { t } = i18n;
  if (status == null) {
    return { label: withheldLabel(withheld, i18n), variant: 'outline' };
  }
  if (parked_by) {
    return {
      label: closedByText(parked_by, 'parked', i18n),
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
  if (status === 'Superseded') {
    return { label: t('result.superseded'), variant: 'outline' };
  }
  if (!verdict) {
    return { label: getStatusLabel(status, t), variant: 'outline' };
  }
  return {
    label: getVerdictLabel(verdict, t),
    variant: getVerdictVariant(verdict),
  };
}

/**
 * What a submission being judged shows beside its spinner. A queued one
 * waits for a judge (no worker may be taking work at all), so it never
 * reads as being judged.
 */
export function progressLabel(
  status: ShownStatus,
  t: I18nContextValue['t'],
): string {
  switch (status) {
    case 'Queued':
      return t('result.waitingForJudge');
    case 'Compiling':
      return t('result.compiling');
    case 'Pending':
    case 'Running':
    case 'Judged':
    case 'CompilationError':
    case 'SystemError':
    case 'Superseded':
    case null:
    case undefined:
      return t('result.judging');
  }
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
