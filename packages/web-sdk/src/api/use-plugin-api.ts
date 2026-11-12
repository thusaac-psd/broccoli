import { use, useMemo } from 'react';

import { ApiClientContext } from '@/api/api-client-context';
import { createPluginApi, type PluginApi } from '@/api/plugin-api';
import { useIdempotentRequest } from '@/hooks/use-idempotent-request';

/**
 * The HTTP API of the plugin `pluginId`, authenticated as the current user.
 * Paths are the plugin's own routes, e.g.
 * `usePluginApi('my-plugin').get('/api/plugins/my-plugin/contests/1/info')`.
 */
export function usePluginApi(pluginId: string): PluginApi {
  const context = use(ApiClientContext);
  if (!context) {
    throw new Error('usePluginApi must be used within an ApiClientProvider');
  }
  const sendIdempotent = useIdempotentRequest();
  const { apiFetch, baseUrl } = context;
  return useMemo(
    () =>
      createPluginApi({ fetch: apiFetch, baseUrl, pluginId, sendIdempotent }),
    [apiFetch, baseUrl, pluginId, sendIdempotent],
  );
}
