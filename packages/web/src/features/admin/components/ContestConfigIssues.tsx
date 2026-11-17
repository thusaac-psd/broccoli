import { useTranslation } from '@broccoli/web-sdk/i18n';

import { ConfigIssuesAlert } from '@/features/admin/components/ConfigIssueList';
import { useContestConfigIssues } from '@/features/admin/hooks/use-contest-config-issues';

/**
 * What the plugins find wrong with the contest's configs and its problems'
 * configs in it, such as too few advancement places for its problems, or a
 * problem its contest type cannot judge yet. Nothing when there is nothing.
 */
export function ContestConfigIssues({
  contestId,
  problems,
  enabled,
}: {
  contestId: number;
  /** The contest's problems, by which the list names them: their labels. */
  problems: readonly { problem_id: number; label: string }[];
  enabled: boolean;
}) {
  const { t } = useTranslation();
  const { data: issues } = useContestConfigIssues(contestId, { enabled });
  const problemLabel = (problemId: number) =>
    problems.find((p) => p.problem_id === problemId)?.label ??
    String(problemId);
  return (
    <ConfigIssuesAlert
      title={t('admin.configIssues.contestTitle')}
      issues={issues}
      of={{ kind: 'contest', problemLabel }}
    />
  );
}
