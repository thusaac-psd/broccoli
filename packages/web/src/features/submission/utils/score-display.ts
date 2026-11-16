import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import {
  type SubmissionOutcome,
  submissionPhase,
} from '@broccoli/web-sdk/submission';

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

/**
 * A submission's score, or what stands in for it: "in progress" only while
 * it is being judged ({@link submissionPhase}), never while it waits for its
 * contest's plugin or its outcome is withheld.
 */
export function getSubmissionScoreDisplay(
  outcome: SubmissionOutcome,
  score: number | null | undefined,
  t: Translate,
): SubmissionScoreDisplay {
  if (outcome.status === 'Judged' && score != null) {
    return { kind: 'score', value: formatScoreValue(score) };
  }
  if (submissionPhase(outcome) === 'judging') {
    return { kind: 'placeholder', label: t('result.scoreInProgress') };
  }
  return { kind: 'placeholder', label: t('result.scoreUnavailable') };
}
