import { use } from 'react';

import { ApiClientContext } from '@/api/api-client-context';

/**
 * The path of the API base URL without a trailing slash (`/api/v1` by
 * default), for API links stored in content, such as attachment URLs in a
 * problem statement, which must not carry this page's origin.
 */
export function useApiBasePath(): string {
  const context = use(ApiClientContext);
  if (!context) {
    throw new Error('useApiBasePath must be used within an ApiClientProvider');
  }
  return new URL(context.baseUrl, 'http://localhost').pathname.replace(
    /\/+$/,
    '',
  );
}
