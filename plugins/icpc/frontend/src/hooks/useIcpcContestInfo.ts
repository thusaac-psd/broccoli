import { useQuery } from '@tanstack/react-query';

import { useIcpcApi } from './useIcpcApi';

/**
 * The ICPC settings of a contest. The slots that read it declare
 * `contest_type = "icpc"`, so they mount only on ICPC contests: a failure here
 * is a failure to show, never a sign that the contest is of another format.
 */
export function useIcpcContestInfo(contestId: number | undefined) {
  const api = useIcpcApi();
  return useQuery({
    queryKey: ['icpc-contest-info', contestId],
    enabled: contestId !== undefined,
    queryFn: () => api.getContestInfo(contestId!),
    staleTime: 5 * 60 * 1000,
  });
}
