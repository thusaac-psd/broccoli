import { useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useQuery } from '@tanstack/react-query';
import { AlertTriangle } from 'lucide-react';

import { ConfigIssueList } from '@/features/admin/components/ConfigIssueList';
import { problemConfigIssuesQueryKey } from '@/features/admin/hooks/test-case-queries';

/**
 * The problem's stored configs that their plugins now refuse, such as
 * subtasks naming a test case that was deleted. Nothing when there are none.
 */
export function ProblemConfigIssues({ problemId }: { problemId: number }) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const { data: issues } = useQuery({
    queryKey: problemConfigIssuesQueryKey(problemId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/problems/{id}/config/issues',
        { params: { path: { id: problemId } } },
      );
      if (error) throw error;
      return data;
    },
  });
  if (!issues || issues.length === 0) return null;

  return (
    <div
      role="alert"
      className="mb-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-300"
    >
      <p className="flex items-center gap-2 font-medium">
        <AlertTriangle className="h-4 w-4 shrink-0" />
        {t('admin.configIssues.title')}
      </p>
      <div className="mt-2">
        <ConfigIssueList issues={issues} />
      </div>
    </div>
  );
}
