import { usePluginApi } from '@broccoli/web-sdk/api';
import { useMemo } from 'react';

import type {
  BracketResponse,
  ForceDecideRequest,
  ForceDecideResponse,
  MatchView,
  OrderRequest,
  OrderResponse,
  SetupRequest,
  SetupResponse,
  StartResponse,
} from '../types.ts';

export const PLUGIN_ID = 'codelink-bracket';
export const ROUTES = '/api/plugins/codelink-bracket';

export function useBracketApi() {
  const api = usePluginApi(PLUGIN_ID);

  return useMemo(
    () => ({
      getBracket: (contestId: number) =>
        api.get<BracketResponse>(`${ROUTES}/contests/${contestId}/bracket`),

      getMatch: (contestId: number, matchId: number) =>
        api.get<MatchView>(
          `${ROUTES}/contests/${contestId}/matches/${matchId}`,
        ),

      submitOrder: (
        contestId: number,
        matchId: number,
        order: OrderRequest['order'],
      ) =>
        api.post<OrderResponse>(
          `${ROUTES}/contests/${contestId}/matches/${matchId}/order`,
          { order } satisfies OrderRequest,
        ),

      startMatch: (contestId: number, matchId: number) =>
        api.post<StartResponse>(
          `${ROUTES}/contests/${contestId}/matches/${matchId}/start`,
        ),

      forceDecide: (
        contestId: number,
        matchId: number,
        winner: number | null,
      ) =>
        api.post<ForceDecideResponse>(
          `${ROUTES}/contests/${contestId}/matches/${matchId}/force-decide`,
          { winner } satisfies ForceDecideRequest,
        ),

      getSetup: (contestId: number) =>
        api.get<SetupResponse>(`${ROUTES}/contests/${contestId}/setup`),

      setup: (contestId: number, body: SetupRequest) =>
        api.post<unknown>(`${ROUTES}/contests/${contestId}/setup`, body),
    }),
    [api],
  );
}
