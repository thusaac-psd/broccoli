import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  describeError,
  errorCodeText,
  type ErrorKind,
  errorKind,
  isTemporaryApiError,
  nothingForThisViewer,
  RequestError,
  retryCanFix,
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

test("an error code a plugin raised reads only from that plugin's pack", () => {
  const catalog: Record<string, string> = {
    'icpc.apiError.NO_TEST_CASES': 'ICPC: no test cases',
    'apiError.NO_TEST_CASES': 'Host: no test cases',
    'apiError.OPERATION_LOST': 'Host: operation lost',
    'apiError.SHARED': 'Host: shared',
  };
  const translator = { lookup: (key: string) => catalog[key] };
  assert.equal(
    errorCodeText('icpc.NO_TEST_CASES', translator),
    'ICPC: no test cases',
  );
  // Another plugin raising the same code without its own text reads neither
  // the first plugin's text nor the host's: the caller's generic message.
  assert.equal(
    errorCodeText('codelink-bracket.NO_TEST_CASES', translator),
    undefined,
  );
  assert.equal(errorCodeText('ioi.SHARED', translator), undefined);
  // A plugin's code named like a host fault never reads as that fault.
  assert.equal(errorCodeText('ioi.OPERATION_LOST', translator), undefined);
  // A host code carries no plugin id.
  assert.equal(
    errorCodeText('OPERATION_LOST', translator),
    'Host: operation lost',
  );
  // No translation anywhere: the caller shows its generic message.
  assert.equal(errorCodeText('icpc.UNKNOWN', translator), undefined);
  assert.equal(errorCodeText('UNKNOWN', translator), undefined);
});

test("a plugin's refusal is described from that plugin's pack only", () => {
  const catalog: Record<string, string> = {
    'apiError.withDetail': '{message} ({detail})',
    'cooldown.apiError.COOLDOWN_ACTIVE': 'Wait {seconds}s',
    'apiError.COOLDOWN_ACTIVE': 'Host text',
    'apiError.PLUGIN_UNAVAILABLE': 'Busy, retrying',
    'apiError.kind.forbidden': 'Not allowed',
  };
  const translator = {
    lookup: (key: string, params?: Record<string, string | number>) =>
      catalog[key]?.replace(/\{(\w+)\}/g, (_, name) => String(params?.[name])),
  };
  const refusal = new RequestError(429, 'cooldown.COOLDOWN_ACTIVE', 'wait', {
    seconds: 5,
  });
  assert.deepEqual(describeError(refusal, 'Failed', translator), {
    kind: 'rateLimited',
    message: 'Wait 5s',
  });
  // A plugin sending a host code gets neither the host's text nor its kind.
  const spoof = new RequestError(403, 'evil.PLUGIN_UNAVAILABLE', 'busy');
  assert.ok(!isTemporaryApiError(spoof));
  assert.deepEqual(describeError(spoof, 'Failed', translator), {
    kind: 'forbidden',
    message: 'Failed (busy)',
  });
});

test("an error answer carries the server's Retry-After in seconds, and nothing else as one", () => {
  const answer = (retryAfter?: string) =>
    new Response('{}', {
      status: 503,
      headers: retryAfter === undefined ? {} : { 'retry-after': retryAfter },
    });
  const body = { code: 'QUEUE_OVERLOADED', message: 'busy' };
  assert.equal(
    RequestError.fromResponse(answer('2'), body).retryAfterMs,
    2_000,
  );
  assert.equal(RequestError.fromResponse(answer(), body).retryAfterMs, null);
  assert.equal(
    RequestError.fromResponse(answer('Wed, 21 Oct 2026 07:28:00 GMT'), body)
      .retryAfterMs,
    null,
  );
});

test('an untranslated refusal of any client-error kind shows what the server said', () => {
  const said = 'Print limit reached (2 jobs per contestant)';
  for (const status of [400, 401, 403, 404, 409, 422, 429]) {
    const refusal = new RequestError(status, 'print.LIMIT_REACHED', said);
    assert.equal(
      describeError(refusal, 'Could not print', translator).message,
      `Could not print (${said})`,
      String(status),
    );
  }
});

test('an untranslated refusal with nothing said reads as its kind or what failed', () => {
  const kinds: Record<string, string> = {
    'apiError.kind.forbidden': 'Not allowed',
    'apiError.kind.notFound': 'Gone',
    'apiError.kind.rateLimited': 'Too many requests',
    'apiError.kind.unauthenticated': 'Sign in',
  };
  const lookup = { lookup: (key: string) => kinds[key] };
  const read = (status: number) =>
    describeError(new RequestError(status, 'x.CODE'), 'Failed', lookup).message;
  assert.equal(read(403), 'Not allowed');
  assert.equal(read(404), 'Gone');
  assert.equal(read(429), 'Too many requests');
  assert.equal(read(401), 'Sign in');
  assert.equal(read(400), 'Failed');
  assert.equal(read(409), 'Failed');
});

test('only a failure a retry can fix offers one', () => {
  const kinds: ErrorKind[] = [
    'network',
    'temporary',
    'unauthenticated',
    'forbidden',
    'notFound',
    'rateLimited',
    'conflict',
    'invalid',
    'server',
  ];
  assert.deepEqual(kinds.filter(retryCanFix), [
    'network',
    'temporary',
    'rateLimited',
    'server',
  ]);
});

test('a refusal of this viewer is nothing to show; any other failure stays', () => {
  for (const status of [403, 404]) {
    assert.equal(nothingForThisViewer(new RequestError(status, 'X')), null);
  }
  for (const err of [new RequestError(500, 'X'), new RequestError(null, 'X')]) {
    assert.throws(
      () => nothingForThisViewer(err),
      (thrown) => thrown === err,
    );
  }
});
