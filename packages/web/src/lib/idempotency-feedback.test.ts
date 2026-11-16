import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  IDEMPOTENCY_KEY_IN_PROGRESS,
  IDEMPOTENCY_OUTCOME_UNKNOWN,
  IDEMPOTENCY_RESPONSE_NOT_STORED,
} from '@broccoli/web-sdk/hooks';

import { unsettledMessage } from './idempotency-feedback.ts';

const t = (key: string) => `t:${key}`;

test('an unknown outcome refetches and asks the user to check', () => {
  let refetched = 0;
  const message = unsettledMessage({ code: IDEMPOTENCY_OUTCOME_UNKNOWN }, t, {
    refetch: () => (refetched += 1),
  });
  assert.equal(message, 't:toast.idempotency.outcomeUnknown');
  assert.equal(refetched, 1);
});

test('an unknown outcome uses the caller message when given', () => {
  assert.equal(
    unsettledMessage({ code: IDEMPOTENCY_OUTCOME_UNKNOWN }, t, {
      outcomeUnknownKey: 'clarification.outcomeUnknown',
    }),
    't:clarification.outcomeUnknown',
  );
});

test('a send still in progress says so without refetching', () => {
  let refetched = 0;
  const message = unsettledMessage({ code: IDEMPOTENCY_KEY_IN_PROGRESS }, t, {
    refetch: () => (refetched += 1),
  });
  assert.equal(message, 't:toast.idempotency.inProgress');
  assert.equal(refetched, 0);
});

test('other errors, already-done included, are not unsettled', () => {
  assert.equal(unsettledMessage({ code: 'VALIDATION_ERROR' }, t), null);
  assert.equal(
    unsettledMessage({ code: IDEMPOTENCY_RESPONSE_NOT_STORED }, t),
    null,
  );
  assert.equal(unsettledMessage(new Error('offline'), t), null);
  assert.equal(unsettledMessage(undefined, t), null);
});
