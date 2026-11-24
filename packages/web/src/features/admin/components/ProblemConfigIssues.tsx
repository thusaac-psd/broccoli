import { useTranslation } from '@broccoli/web-sdk/i18n';

import { ConfigIssuesAlert } from '@/features/admin/components/ConfigIssueList';
import { useProblemConfigIssues } from '@/features/admin/hooks/use-problem-config-issues';

/**
 * What the plugins find wrong with the problem's configs, such as subtasks
 * naming a test case that was deleted. Nothing when there is nothing.
 */
export function ProblemConfigIssues({ problemId }: { problemId: number }) {
  const { t } = useTranslation();
  const { data: issues } = useProblemConfigIssues(problemId);
  return (
    <ConfigIssuesAlert
      title={t('admin.configIssues.title')}
      issues={issues}
      of="problem"
    />
  );
}
