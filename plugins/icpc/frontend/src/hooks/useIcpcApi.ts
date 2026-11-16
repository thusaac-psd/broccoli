import { usePluginApi } from '@broccoli/web-sdk/api';
import { useMemo } from 'react';

import type { IcpcContestInfoResponse, StandingsResponse } from '../types';

const ROUTES = '/api/plugins/icpc';

export function useIcpcApi() {
  const api = usePluginApi('icpc');

  return useMemo(
    () => ({
      getContestInfo: (contestId: number) =>
        api.get<IcpcContestInfoResponse>(
          `${ROUTES}/contests/${contestId}/info`,
        ),

      getStandings: (contestId: number) =>
        api.get<StandingsResponse>(`${ROUTES}/contests/${contestId}/standings`),

      reveal: (contestId: number) =>
        api.post<{ revealed: boolean }>(
          `${ROUTES}/contests/${contestId}/reveal`,
        ),
    }),
    [api],
  );
}
