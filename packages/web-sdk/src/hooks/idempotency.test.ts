import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  createIdempotencyKeys,
  fingerprint,
  IDEMPOTENCY_KEY_MISMATCH,
  sendIdempotent,
} from './idempotency.ts';

function counterKeys() {
  let n = 0;
  return createIdempotencyKeys(() => `key-${++n}`);
}

// An openapi-fetch style result.
function result(status: number, error?: unknown) {
  return {
    data: status < 400 ? { id: 1 } : undefined,
    error,
    response: new Response(null, { status }),
  };
}

const submission = (code: string, language = 'cpp') => ({
  contestId: 3,
  problemId: 12,
  language,
  files: [{ filename: 'main.cpp', content: code }],
});

test('fingerprints ignore key order and undefined fields', () => {
  assert.equal(
    fingerprint({ a: 1, b: [1, 2], c: undefined }),
    fingerprint({ b: [1, 2], a: 1 }),
  );
  assert.notEqual(fingerprint({ a: [1, 2] }), fingerprint({ a: [2, 1] }));
  assert.notEqual(fingerprint({ a: '1' }), fingerprint({ a: 1 }));
  assert.notEqual(
    fingerprint(submission('int main(){}')),
    fingerprint(submission('int main(){ }')),
  );
  assert.notEqual(
    fingerprint(new File(['x'], 'a.txt')),
    fingerprint(new File(['xy'], 'a.txt')),
  );
  const form = (name: string) => {
    const data = new FormData();
    data.append('file', new File(['x'], name, { lastModified: 1 }));
    return data;
  };
  assert.equal(fingerprint(form('a.txt')), fingerprint(form('a.txt')));
  assert.notEqual(fingerprint(form('a.txt')), fingerprint(form('b.txt')));
});

test('retrying the identical operation after a failure reuses the key', async () => {
  const keys = counterKeys();
  const sent: string[] = [];
  const send = (status: number) =>
    sendIdempotent(keys, submission('v1'), async (key) => {
      sent.push(key);
      return result(
        status,
        status >= 400 ? { code: 'INTERNAL_ERROR' } : undefined,
      );
    });

  await send(502);
  await assert.rejects(
    sendIdempotent(keys, submission('v1'), async (key) => {
      sent.push(key);
      throw new TypeError('Failed to fetch');
    }),
  );
  await send(201);
  assert.deepEqual(sent, ['key-1', 'key-1', 'key-1']);
});

test('an edited payload never reuses the key of a lost submission', async () => {
  const keys = counterKeys();
  const sent: string[] = [];
  const lost = async (key: string) => {
    sent.push(key);
    throw new TypeError('Failed to fetch');
  };
  await assert.rejects(sendIdempotent(keys, submission('v1'), lost));
  await assert.rejects(sendIdempotent(keys, submission('v2'), lost));
  await assert.rejects(sendIdempotent(keys, submission('v2', 'java'), lost));
  // Going back to v1 is the same operation as the lost one: replaying it is
  // exactly what the server should do.
  await assert.rejects(sendIdempotent(keys, submission('v1'), lost));
  assert.deepEqual(sent, ['key-1', 'key-2', 'key-3', 'key-1']);
});

test('an accepted operation retires its key', async () => {
  const keys = counterKeys();
  const sent: string[] = [];
  for (let i = 0; i < 2; i += 1) {
    await sendIdempotent(keys, submission('v1'), async (key) => {
      sent.push(key);
      return result(201);
    });
  }
  // Plain-data style requests count as accepted when they resolve.
  await sendIdempotent(keys, submission('v1'), async (key) => {
    sent.push(key);
    return { id: 9 };
  });
  assert.deepEqual(sent, ['key-1', 'key-2', 'key-3']);
});

test('a key mismatch is resent once with a fresh key', async () => {
  const keys = counterKeys();
  const sent: string[] = [];
  const mismatch = { code: IDEMPOTENCY_KEY_MISMATCH, message: 'body differs' };

  const ok = await sendIdempotent(keys, submission('v1'), async (key) => {
    sent.push(key);
    return sent.length === 1 ? result(422, mismatch) : result(201);
  });
  assert.equal(ok.response.status, 201);
  assert.deepEqual(sent, ['key-1', 'key-2']);

  // Thrown (mutationFn style) mismatches too; a second one is surfaced.
  await assert.rejects(
    sendIdempotent(keys, submission('v2'), async (key) => {
      sent.push(key);
      throw mismatch;
    }),
    (error) => error === mismatch,
  );
  assert.deepEqual(sent, ['key-1', 'key-2', 'key-3', 'key-4']);
});
