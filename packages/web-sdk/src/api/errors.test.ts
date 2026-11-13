import assert from 'node:assert/strict';
import { test } from 'node:test';

import { isTemporaryApiError } from './errors.ts';

test('a busy plugin or a full queue is temporary', () => {
  for (const code of [
    'PLUGIN_UNAVAILABLE',
    'QUEUE_OVERLOADED',
    'CONTEST_TYPE_UNAVAILABLE',
  ]) {
    assert.ok(isTemporaryApiError({ code, message: 'busy' }), code);
  }
});

test('a refusal or a crash is not temporary', () => {
  assert.ok(!isTemporaryApiError({ code: 'NOT_FOUND', message: 'gone' }));
  assert.ok(!isTemporaryApiError({ code: 'INTERNAL_ERROR', message: 'x' }));
  assert.ok(!isTemporaryApiError(new Error('network')));
  assert.ok(!isTemporaryApiError(null));
});
