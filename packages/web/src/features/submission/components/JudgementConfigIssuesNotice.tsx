import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { PROBLEM_EDIT } from '@broccoli/web-sdk/permissions';
import { AlertTriangle } from 'lucide-react';

import { ConfigIssueList } from '@/features/admin/components/ConfigIssueList';
import { useProblemConfigIssues } from '@/features/admin/hooks/use-problem-config-issues';

/**
 * For staff: the shown judgement was dispatched while its problem's stored
 * configs had issues, so its score may come from a config its plugin
 * refuses. Lists the problem's issues as they stand now to whoever may read
 * them (`problem:edit`), or says they are fixed and a rejudge scores it
 * with the fixed configs.
 */
export function JudgementConfigIssuesNotice({
  problemId,
}: {
  problemId: number;
}) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const canReadIssues = !!user?.permissions.includes(PROBLEM_EDIT);
  const { data: issues } = useProblemConfigIssues(problemId, {
    enabled: canReadIssues,
  });

  return (
    <div
      role="alert"
      className="mt-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-300"
    >
      <p className="flex items-center gap-2 font-medium">
        <AlertTriangle className="h-4 w-4 shrink-0" />
        {t('submissionDetail.configIssues.judged')}
      </p>
      {issues && issues.length > 0 && (
        <div className="mt-2 space-y-2">
          <p>{t('submissionDetail.configIssues.current')}</p>
          <ConfigIssueList issues={issues} />
        </div>
      )}
      {issues && issues.length === 0 && (
        <p className="mt-2">{t('submissionDetail.configIssues.fixed')}</p>
      )}
    </div>
  );
}
