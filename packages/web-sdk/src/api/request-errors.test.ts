import assert from 'node:assert/strict';
import { test } from 'node:test';

import { errorKind, RequestError } from './errors.ts';
import { withRequestErrors } from './request-errors.ts';

type Result = { data?: unknown; error?: unknown; response: Response };

function client(reply: () => Promise<Result>) {
  return withRequestErrors({ GET: (_path: string) => reply() });
}

test('an error answer carries its status and the body fields', async () => {
  const { error } = await client(async () => ({
    error: { code: 'NOT_FOUND', message: 'Submission not found' },
    response: new Response(null, { status: 404 }),
  })).GET('/x');
  assert.ok(error instanceof RequestError);
  assert.equal(error.status, 404);
  assert.equal(error.code, 'NOT_FOUND');
  assert.equal(error.message, 'Submission not found');
  assert.equal(errorKind(error), 'notFound');
});

test('an answer that is not an error body is an HTTP error with its status', async () => {
  const { error } = await client(async () => ({
    error: '<html>Bad Gateway</html>',
    response: new Response(null, { status: 502 }),
  })).GET('/x');
  assert.ok(error instanceof RequestError);
  assert.equal(error.code, 'HTTP_ERROR');
  assert.equal(errorKind(error), 'server');
});

test('no answer at all rejects as a network error', async () => {
  await assert.rejects(
    client(() => Promise.reject(new TypeError('Failed to fetch'))).GET('/x'),
    (error: unknown) => {
      assert.ok(error instanceof RequestError);
      assert.equal(error.status, null);
      assert.equal(errorKind(error), 'network');
      return true;
    },
  );
});

test('a cancelled request stays an abort', async () => {
  const abort = new DOMException('aborted', 'AbortError');
  await assert.rejects(client(() => Promise.reject(abort)).GET('/x'), abort);
});

test('a success passes through untouched', async () => {
  const reply = { data: { id: 1 }, response: new Response(null) };
  assert.equal(await client(async () => reply).GET('/x'), reply);
});
