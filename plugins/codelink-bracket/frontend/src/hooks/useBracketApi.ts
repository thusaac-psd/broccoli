import { useApiFetch } from '@broccoli/web-sdk/api';
import { useCallback, useMemo } from 'react';

import type {
  BracketResponse,
  ForceDecideRequest,
  ForceDecideResponse,
  MatchView,
  OrderRequest,
  OrderResponse,
  StartResponse,
} from '../types.ts';

const PLUGIN_BASE = '/api/v1/p/codelink-bracket/api/plugins/codelink-bracket';

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

export function useBracketApi() {
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
      getBracket: (contestId: number) =>
        fetchJson<BracketResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/bracket`,
        ),

      getMatch: (contestId: number, matchId: number) =>
        fetchJson<MatchView>(
          `${PLUGIN_BASE}/contests/${contestId}/matches/${matchId}`,
        ),

      submitOrder: (
        contestId: number,
        matchId: number,
        order: OrderRequest['order'],
      ) =>
        fetchJson<OrderResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/matches/${matchId}/order`,
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ order } satisfies OrderRequest),
          },
        ),

      startMatch: (contestId: number, matchId: number) =>
        fetchJson<StartResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/matches/${matchId}/start`,
          { method: 'POST' },
        ),

      forceDecide: (
        contestId: number,
        matchId: number,
        winner: number | null,
      ) =>
        fetchJson<ForceDecideResponse>(
          `${PLUGIN_BASE}/contests/${contestId}/matches/${matchId}/force-decide`,
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ winner } satisfies ForceDecideRequest),
          },
        ),
    }),
    [fetchJson],
  );
}
