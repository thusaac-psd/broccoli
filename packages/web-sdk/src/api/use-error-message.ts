import { useCallback } from 'react';

import { describeError, type ErrorDescription } from '@/api/errors';
import { useTranslation } from '@/i18n/hooks';

/**
 * `describeError` bound to the UI language: `(error, failure) => text`,
 * where `failure` is the translated sentence for what went wrong ("Could not
 * save the problem"). Every failure the UI shows goes through it, so the
 * user never reads the server's English.
 */
export function useErrorMessage(): (err: unknown, failure: string) => string {
  const describe = useDescribeError();
  return useCallback(
    (err: unknown, failure: string) => describe(err, failure).message,
    [describe],
  );
}

/** Like `useErrorMessage`, with the failure's `ErrorKind` as well. */
export function useDescribeError(): (
  err: unknown,
  failure: string,
) => ErrorDescription {
  const { lookup } = useTranslation();
  return useCallback(
    (err: unknown, failure: string) => describeError(err, failure, { lookup }),
    [lookup],
  );
}
