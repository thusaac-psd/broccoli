import { useQuery } from '@tanstack/react-query';

import { useIoiApi } from './useIoiApi';

/**
 * The IOI settings of a contest. The slots that read it declare
 * `contest_type = "ioi"`, so they mount only on IOI contests: a failure here
 * is a failure to show, never a sign that the contest is of another format.
 */
export function useIoiContestInfo(contestId: number | undefined) {
  const api = useIoiApi();
  return useQuery({
    queryKey: ['ioi-contest-info', contestId],
    enabled: contestId !== undefined,
    queryFn: () => api.getContestInfo(contestId!),
    staleTime: 5 * 60 * 1000,
  });
}
