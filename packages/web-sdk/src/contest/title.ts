import type { ContestProblem } from '@/contest/types';
import type { I18nContextValue } from '@/i18n';

/**
 * A contest problem row's title, or what stands in for it while the
 * contest's plugin withholds what the problem holds (`withheld`).
 */
export function contestProblemTitle(
  problem: Pick<ContestProblem, 'problem_title'>,
  t: I18nContextValue['t'],
): string {
  return problem.problem_title ?? t('problems.titleWithheld');
}
