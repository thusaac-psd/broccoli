// What to tell the user when an idempotent send (`useIdempotentRequest`) is
// refused in a way that is not a failure of the operation itself. Already-done
// refusals are successes that callers handle first; this covers the other two.

import {
  idempotencyOutcome,
  unsettledMessageKey,
} from '@broccoli/web-sdk/hooks';
import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import { toast } from 'sonner';

import type { MessageKey } from './i18n/catalog.ts';

type Translate = I18nContextValue['t'];

export interface UnsettledOptions {
  /**
   * Refetch what the operation would have changed, so the screen shows
   * whether it happened. Run only when the outcome is unknown.
   */
  refetch?: () => unknown;
  /**
   * Message for an unknown outcome, ideally saying where to check. Defaults
   * to a generic one.
   */
  outcomeUnknownKey?: MessageKey;
}

/**
 * The message for an unsettled idempotent send, or null for any other error
 * (report that as a failure as usual).
 *
 * - Outcome unknown: an earlier attempt was cut off and may have taken
 *   effect. `refetch` runs and the message asks the user to check before
 *   retrying; a retry gets a fresh key, so it runs as a new operation.
 * - In progress: an earlier attempt is still running. Its key is kept, so
 *   retrying the identical operation in a moment gets its result.
 */
export function unsettledMessage(
  error: unknown,
  t: Translate,
  { refetch, outcomeUnknownKey }: UnsettledOptions = {},
): string | null {
  switch (idempotencyOutcome(error)) {
    case 'outcome-unknown':
      refetch?.();
      return t(outcomeUnknownKey ?? unsettledMessageKey('outcome-unknown'));
    case 'in-progress':
      return t(unsettledMessageKey('in-progress'));
    default:
      return null;
  }
}

/**
 * `unsettledMessage` shown as a toast. Returns whether `error` was unsettled,
 * in which case the caller reports nothing else.
 */
export function toastUnsettled(
  error: unknown,
  t: Translate,
  options?: UnsettledOptions,
): boolean {
  const message = unsettledMessage(error, t, options);
  if (message === null) return false;
  if (idempotencyOutcome(error) === 'in-progress') toast.info(message);
  else toast.warning(message);
  return true;
}
