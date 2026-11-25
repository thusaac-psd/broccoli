import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { SUBMISSION_VIEW_ALL } from '@broccoli/web-sdk/permissions';
import { submissionViewKeys } from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

/**
 * Whether the judgement was dispatched while its problem's stored configs
 * had issues. The server tells only viewers who see every submission
 * (`submission:view_all`), so nobody else asks.
 */
export function useJudgementConfigIssues(
  submissionId: number,
  judgementId: number | null | undefined,
): boolean {
  const apiClient = useApiClient();
  const { user } = useAuth();
  const enabled =
    judgementId != null && !!user?.permissions.includes(SUBMISSION_VIEW_ALL);
  const { data } = useQuery({
    queryKey: submissionViewKeys.judgement(submissionId, judgementId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/submissions/{id}/judgements/{judgement_id}',
        {
          params: {
            path: { id: submissionId, judgement_id: judgementId as number },
            query: { per_page: 1 },
          },
        },
      );
      if (error) throw error;
      return data;
    },
    enabled,
  });
  return data?.config_issues === true;
}
