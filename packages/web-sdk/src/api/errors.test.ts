import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  describeError,
  errorKind,
  isTemporaryApiError,
  judgeErrorText,
  RequestError,
  TranslatedError,
} from './errors.ts';

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
  assert.ok(
    !isTemporaryApiError({ code: 'CONTEST_TYPE_DISABLED', message: 'closed' }),
  );
  assert.ok(!isTemporaryApiError({ code: 'INTERNAL_ERROR', message: 'x' }));
  assert.ok(!isTemporaryApiError(new Error('network')));
  assert.ok(!isTemporaryApiError(null));
});

test('the kind of a failure follows its status', () => {
  const kind = (status: number) =>
    errorKind(new RequestError(status, 'SOMETHING'));
  assert.equal(kind(401), 'unauthenticated');
  assert.equal(kind(403), 'forbidden');
  assert.equal(kind(404), 'notFound');
  assert.equal(kind(409), 'conflict');
  assert.equal(kind(429), 'rateLimited');
  assert.equal(kind(400), 'invalid');
  assert.equal(kind(422), 'invalid');
  assert.equal(kind(500), 'server');
  assert.equal(kind(503), 'temporary');
  assert.equal(errorKind(RequestError.network()), 'network');
  assert.equal(errorKind(new TypeError('Failed to fetch')), 'network');
  assert.equal(errorKind(new Error('boom')), 'server');
});

const catalog: Record<string, string> = {
  'apiError.USERNAME_TAKEN': 'That username is taken',
  'apiError.LIMIT': 'Limit reached ({made}/{max})',
  'apiError.kind.network': 'The server cannot be reached',
  'apiError.kind.server': 'The server had a problem',
  'apiError.withDetail': '{message} ({detail})',
};
const translator = {
  lookup: (key: string, params?: Record<string, string | number>) => {
    const text = catalog[key];
    return text?.replace(/\{(\w+)\}/g, (_, name) => String(params?.[name]));
  },
};

test('a translated code wins, with the details as parameters', () => {
  const err = new RequestError(429, 'LIMIT', 'Submission limit reached', {
    made: 3,
    max: 3,
  });
  assert.equal(
    describeError(err, 'Failed', translator).message,
    'Limit reached (3/3)',
  );
  assert.equal(
    describeError(
      new RequestError(409, 'USERNAME_TAKEN', 'Username is already taken'),
      'Failed',
      translator,
    ).message,
    'That username is taken',
  );
});

test('a list in the details is a parameter of its items, comma separated', () => {
  const taken = new RequestError(
    409,
    'USERNAME_TAKEN',
    'Usernames already taken: ann, bob',
    { usernames: ['ann', 'bob'] },
  );
  const names = {
    lookup: (key: string, params?: Record<string, string | number>) =>
      key === 'apiError.USERNAME_TAKEN'
        ? `Taken: ${String(params?.usernames)}`
        : undefined,
  };
  assert.equal(
    describeError(taken, 'Failed', names).message,
    'Taken: ann, bob',
  );
});

test('an untranslated refusal says what failed, with the server text as detail', () => {
  const err = new RequestError(400, 'VALIDATION_ERROR', 'Title too long');
  assert.equal(
    describeError(err, 'Could not save', translator).message,
    'Could not save (Title too long)',
  );
});

test('no answer and server crashes get the translated kind message, never the raw text', () => {
  assert.equal(
    describeError(new TypeError('Failed to fetch'), 'Failed', translator)
      .message,
    'The server cannot be reached',
  );
  assert.equal(
    describeError(
      new RequestError(500, 'INTERNAL_ERROR', 'An unexpected error occurred'),
      'Failed',
      translator,
    ).message,
    'The server had a problem',
  );
});

test('an already translated error is shown as it is', () => {
  assert.equal(
    describeError(new TranslatedError('已取消'), 'Failed', translator).message,
    '已取消',
  );
});

test("a judge error code a plugin raised reads from that plugin's pack first", () => {
  const catalog: Record<string, string> = {
    'icpc.apiError.NO_TEST_CASES': 'ICPC: no test cases',
    'apiError.NO_TEST_CASES': 'Host: no test cases',
    'apiError.OPERATION_LOST': 'Host: operation lost',
    'apiError.SHARED': 'Host: shared',
  };
  const translator = { lookup: (key: string) => catalog[key] };
  assert.equal(
    judgeErrorText('icpc.NO_TEST_CASES', translator),
    'ICPC: no test cases',
  );
  // Another plugin raising the same code without its own text falls back
  // to the host catalog, never to the first plugin's text.
  assert.equal(
    judgeErrorText('codelink-bracket.NO_TEST_CASES', translator),
    'Host: no test cases',
  );
  assert.equal(judgeErrorText('ioi.SHARED', translator), 'Host: shared');
  // A host code carries no plugin id.
  assert.equal(
    judgeErrorText('OPERATION_LOST', translator),
    'Host: operation lost',
  );
  // No translation anywhere: the caller shows its generic message.
  assert.equal(judgeErrorText('icpc.UNKNOWN', translator), undefined);
  assert.equal(judgeErrorText('UNKNOWN', translator), undefined);
});
