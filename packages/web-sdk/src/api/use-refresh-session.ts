import { use } from 'react';

import { ApiClientContext } from '@/api/api-client-context';

export function useRefreshSession() {
  const context = use(ApiClientContext);
  if (!context) {
    throw new Error(
      'useRefreshSession must be used within an ApiClientProvider',
    );
  }
  return context.refreshSession;
}
