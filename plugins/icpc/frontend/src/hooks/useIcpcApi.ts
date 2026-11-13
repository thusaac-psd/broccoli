import { useApiFetch } from '@broccoli/web-sdk/api';
import { useCallback, useMemo } from 'react';

import type { IcpcContestInfoResponse, StandingsResponse } from '../types';

const PLUGIN_BASE = '/api/v1/p/icpc/api/plugins/icpc';

export class ApiError extends Error {
  status: number;
  code?: string;

  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

export function useIcpcApi() {
  const apiFetch = useApiFetch();

  const fetchJson = useCallback(
    async <T>(path: string, init?: RequestInit): Promise<T> => {
      const res = await apiFetch(path, init);
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new ApiError(
          body.error || body.message || `Request failed: ${res.status}`,
          res.status,
          body.code,
        );
      }
      return res.json();
    },
    [apiFetch],
  );

  return useMemo(
    () => ({
      getContestInfo: (contestId: number) =>
        fetchJson<IcpcContestInfoResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/info`,
        ),

      getStandings: (contestId: number) =>
        fetchJson<StandingsResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/standings`,
        ),

      reveal: (contestId: number) =>
        fetchJson<{ revealed: boolean }>(
          `${PLUGIN_BASE}/contests/${contestId}/reveal`,
          { method: 'POST' },
        ),
    }),
    [fetchJson],
  );
}
