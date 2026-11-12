import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  createIdempotencyKeys,
  IDEMPOTENCY_OUTCOME_UNKNOWN,
  sendIdempotent,
} from '../hooks/idempotency.ts';
import { createPluginApi, PluginApiError } from './plugin-api.ts';

interface Sent {
  url: string;
  init?: RequestInit;
}

function json(status: number, body?: unknown): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
  });
}

function client(replies: Response[]) {
  const sent: Sent[] = [];
  let n = 0;
  const keys = createIdempotencyKeys(() => `key-${++n}`);
  const api = createPluginApi({
    fetch: async (url, init) => {
      sent.push({ url, init });
      const reply = replies.shift();
      if (!reply) throw new TypeError('Failed to fetch');
      return reply;
    },
    baseUrl: 'https://oj.test/api/v1/',
    pluginId: 'demo',
    sendIdempotent: (operation, request) =>
      sendIdempotent(keys, operation, request),
  });
  return { api, sent };
}

function keyOf(sent: Sent): string | undefined {
  return (sent.init?.headers as Record<string, string> | undefined)?.[
    'Idempotency-Key'
  ];
}

test('get resolves plugin paths under the plugin prefix', async () => {
  const { api, sent } = client([json(200, { ok: true })]);
  assert.deepEqual(await api.get('/api/plugins/demo/info?x=1'), { ok: true });
  assert.equal(
    sent[0].url,
    'https://oj.test/api/v1/p/demo/api/plugins/demo/info?x=1',
  );
});

test('an empty reply body resolves to undefined', async () => {
  const { api } = client([new Response(null, { status: 204 })]);
  assert.equal(await api.post('/act'), undefined);
});

test('a failed request rejects with its status, code and message', async () => {
  const { api } = client([
    json(409, { code: 'TOKEN_EXHAUSTED', message: 'No tokens left' }),
  ]);
  await assert.rejects(api.post('/use'), (error: unknown) => {
    assert.ok(error instanceof PluginApiError);
    assert.equal(error.status, 409);
    assert.equal(error.code, 'TOKEN_EXHAUSTED');
    assert.equal(error.message, 'No tokens left');
    return true;
  });
});

test('a plugin error body with only an error string is its message', async () => {
  const { api } = client([json(400, { error: 'bad order' })]);
  await assert.rejects(api.get('/x'), { message: 'bad order', status: 400 });
});

test('post sends JSON and retries of the same request reuse its key', async () => {
  const { api, sent } = client([json(502), json(200, { id: 1 })]);
  await assert.rejects(api.post('/jobs', { file: 'a.cpp' }));
  assert.deepEqual(await api.post('/jobs', { file: 'a.cpp' }), { id: 1 });
  assert.equal(sent[0].init?.body, JSON.stringify({ file: 'a.cpp' }));
  assert.equal(keyOf(sent[0]), 'key-1');
  assert.equal(keyOf(sent[1]), 'key-1');
});

test('a different body gets a fresh key', async () => {
  const { api, sent } = client([json(502), json(200, {})]);
  await assert.rejects(api.post('/jobs', { file: 'a.cpp' }));
  await api.post('/jobs', { file: 'b.cpp' });
  assert.notEqual(keyOf(sent[0]), keyOf(sent[1]));
});

test('an unknown outcome is passed on with its idempotency code', async () => {
  const { api } = client([
    json(409, { code: IDEMPOTENCY_OUTCOME_UNKNOWN, message: 'unknown' }),
  ]);
  await assert.rejects(api.post('/use'), { code: IDEMPOTENCY_OUTCOME_UNKNOWN });
});
