import {
  type IdempotentResult,
  unsettledMessageKey,
} from '@broccoli/web-sdk/hooks';
import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import { toast } from '@broccoli/web-sdk/ui';

import type { SubmitResult } from '../types';

/**
 * A print whose send may or may not have queued the job: the dialog stays
 * open and says so instead of inviting a second print.
 */
type Unsettled = Extract<IdempotentResult<unknown>, { settled: false }>;

class UnsettledPrint extends Error {
  readonly outcome: Unsettled['outcome'];
  constructor(outcome: Unsettled['outcome']) {
    super(outcome);
    this.outcome = outcome;
  }
}

/**
 * Announce a queued print with the page count the server estimated, or throw
 * when the send did not settle.
 */
export function announcePrinted(
  result: IdempotentResult<SubmitResult>,
  t: I18nContextValue['t'],
): void {
  if (!result.settled) throw new UnsettledPrint(result.outcome);
  const queued = result.data;
  toast.success(
    queued?.status === 'pending_approval'
      ? t('print.action.needsApproval')
      : t('print.action.submitted'),
    queued && {
      description: t('print.submission.files', {
        count: queued.jobs,
        pages: queued.pages,
      }),
    },
  );
}

/** What a failed print dialog says. */
export function printFailure(
  error: unknown,
  t: I18nContextValue['t'],
  errorMessage: (error: unknown, fallback: string) => string,
): string {
  return error instanceof UnsettledPrint
    ? t(unsettledMessageKey(error.outcome))
    : errorMessage(error, t('print.action.failed'));
}
