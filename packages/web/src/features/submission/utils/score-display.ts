import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import type { SubmissionStatus } from '@broccoli/web-sdk/submission';

type Translate = I18nContextValue['t'];

export type SubmissionScoreDisplay =
  | { kind: 'score'; value: string }
  | { kind: 'placeholder'; label: string };

export function formatScoreValue(value: number): string {
  if (!Number.isFinite(value)) return String(value);
  const rounded = Math.round(value * 100) / 100;
  if (Number.isInteger(rounded)) return String(rounded);
  return rounded
    .toFixed(2)
    .replace(/\.0+$/, '')
    .replace(/(\.\d*[1-9])0+$/, '$1');
}

export function getSubmissionScoreDisplay(
  status: SubmissionStatus,
  score: number | null | undefined,
  t: Translate,
): SubmissionScoreDisplay {
  if (status === 'Judged' && score != null) {
    return { kind: 'score', value: formatScoreValue(score) };
  }

  if (
    status === 'Queued' ||
    status === 'Pending' ||
    status === 'Compiling' ||
    status === 'Running'
  ) {
    return { kind: 'placeholder', label: t('result.scoreInProgress') };
  }

  return { kind: 'placeholder', label: t('result.scoreUnavailable') };
}
