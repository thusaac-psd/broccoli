import { use } from 'react';

import { ApiClientContext } from '@/api/api-client-context';

/** The sign-in session store. Components read it through `useAuth`. */
export function useSession() {
  const context = use(ApiClientContext);
  if (!context) {
    throw new Error('useSession must be used within an ApiClientProvider');
  }
  return context.session;
}
