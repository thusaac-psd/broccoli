import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  createIdempotencyKeys,
  IDEMPOTENCY_OUTCOME_UNKNOWN,
  IDEMPOTENCY_RESPONSE_NOT_STORED,
  sendIdempotent,
  settleIdempotent,
} from '../hooks/idempotency.ts';
import { createPluginApi, PluginApiError } from './plugin-api.ts';

interface Sent {
  url: string;
  init?: RequestInit;
}

function json(status: number, body?: unknown): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
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
    sendSettled: (operation, request) =>
      settleIdempotent(sendIdempotent(keys, operation, request)),
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

test('an empty reply settles without data', async () => {
  const { api } = client([new Response(null, { status: 204 })]);
  assert.deepEqual(await api.post('/act'), { settled: true, data: undefined });
});

test('a JSON reply without a content type is parsed', async () => {
  const { api } = client([new Response('{"ok":true}', { status: 200 })]);
  assert.deepEqual(await api.get('/info'), { ok: true });
});

test('a reply claiming JSON that is not is an invalid response', async () => {
  const { api } = client([
    new Response('oops', {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }),
  ]);
  await assert.rejects(api.get('/info'), { code: 'INVALID_RESPONSE' });
});

test('a non-JSON reply is read as text', async () => {
  const { api } = client([new Response('pong', { status: 200 })]);
  assert.equal(await api.get('/ping'), 'pong');
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

test('a failure without a message has an empty one', async () => {
  const { api } = client([new Response(null, { status: 502 })]);
  await assert.rejects(api.get('/x'), { message: '', status: 502 });
});

test('a plugin error carries the host error shape', async () => {
  const { api } = client([
    json(400, { code: 'VALIDATION_ERROR', message: 'bad order' }),
  ]);
  await assert.rejects(api.get('/x'), {
    message: 'bad order',
    status: 400,
    code: 'VALIDATION_ERROR',
  });
});

test('post sends JSON and retries of the same request reuse its key', async () => {
  const { api, sent } = client([json(502), json(200, { id: 1 })]);
  await assert.rejects(api.post('/jobs', { file: 'a.cpp' }));
  assert.deepEqual(await api.post('/jobs', { file: 'a.cpp' }), {
    settled: true,
    data: { id: 1 },
  });
  assert.equal(sent[0].init?.body, JSON.stringify({ file: 'a.cpp' }));
  assert.equal(keyOf(sent[0]), 'key-1');
  assert.equal(keyOf(sent[1]), 'key-1');
});

test('post sends form data as it is', async () => {
  const { api, sent } = client([json(200, {})]);
  const form = new FormData();
  form.append('file', 'x');
  await api.post('/upload', form);
  assert.equal(sent[0].init?.body, form);
  assert.equal(
    (sent[0].init?.headers as Record<string, string>)['Content-Type'],
    undefined,
  );
});

test('a different body gets a fresh key', async () => {
  const { api, sent } = client([json(502), json(200, {})]);
  await assert.rejects(api.post('/jobs', { file: 'a.cpp' }));
  await api.post('/jobs', { file: 'b.cpp' });
  assert.notEqual(keyOf(sent[0]), keyOf(sent[1]));
});

test('an already-done post settles; an unknown outcome does not', async () => {
  const { api } = client([
    json(409, { code: IDEMPOTENCY_RESPONSE_NOT_STORED, message: 'x' }),
    json(409, { code: IDEMPOTENCY_OUTCOME_UNKNOWN, message: 'x' }),
  ]);
  assert.deepEqual(await api.post('/a'), { settled: true, data: undefined });
  assert.deepEqual(await api.post('/b'), {
    settled: false,
    outcome: 'outcome-unknown',
  });
});
