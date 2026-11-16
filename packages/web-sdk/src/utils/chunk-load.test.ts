import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  failedModuleUrl,
  isChunkLoadError,
  isRetryableChunkLoadError,
  retryLoad,
  retryUrl,
} from './chunk-load.ts';

test('chunk-load failures are recognised across browsers', () => {
  for (const message of [
    // Chromium
    'Failed to fetch dynamically imported module: https://oj.lan/assets/CodeEditor-x1.js',
    // Firefox
    'error loading dynamically imported module: https://oj.lan/assets/CodeEditor-x1.js',
    // Safari
    'Importing a module script failed.',
    "'text/html' is not a valid JavaScript MIME type.",
    // Vite's preload helper
    'Unable to preload CSS for /assets/CodeEditor-x1.css',
  ]) {
    assert.ok(isChunkLoadError(new TypeError(message)), message);
  }
  const named = new Error('Loading chunk 7 failed.');
  named.name = 'ChunkLoadError';
  assert.ok(isChunkLoadError(named));
});

test('other errors are not chunk-load failures', () => {
  assert.ok(
    !isChunkLoadError(
      new TypeError("Cannot read properties of undefined (reading 'id')"),
    ),
  );
  assert.ok(!isChunkLoadError(new TypeError('Failed to fetch')));
  assert.ok(!isChunkLoadError('Failed to fetch dynamically imported module'));
  assert.ok(!isChunkLoadError(null));
});

const chunkError = () =>
  new TypeError('Failed to fetch dynamically imported module: /a.js');

function harness() {
  const sleeps: number[] = [];
  let onlineWaits = 0;
  return {
    sleeps,
    onlineWaits: () => onlineWaits,
    options: {
      sleep: async (ms: number) => {
        sleeps.push(ms);
      },
      whenOnline: async () => {
        onlineWaits += 1;
      },
    },
  };
}

test('a failed chunk load is retried with backoff until it loads', async () => {
  const h = harness();
  const attempts: number[] = [];
  const value = await retryLoad(async (attempt) => {
    attempts.push(attempt);
    if (attempt < 2) throw chunkError();
    return 'module';
  }, h.options);
  assert.equal(value, 'module');
  assert.deepEqual(attempts, [0, 1, 2]);
  assert.deepEqual(h.sleeps, [1_000, 2_000]);
  assert.equal(h.onlineWaits(), 2);
});

test('retries stop at the attempt limit with the last error', async () => {
  const h = harness();
  let calls = 0;
  await assert.rejects(
    retryLoad(
      async () => {
        calls += 1;
        throw chunkError();
      },
      { ...h.options, attempts: 3 },
    ),
    /Failed to fetch dynamically imported module/,
  );
  assert.equal(calls, 3);
});

test('a bug in the loaded code is not retried by default', async () => {
  const h = harness();
  let calls = 0;
  await assert.rejects(
    retryLoad(async () => {
      calls += 1;
      throw new TypeError('x is not a function');
    }, h.options),
  );
  assert.equal(calls, 1);
});

test('the failed module is named by its URL, and every retry gets a new one', () => {
  const chromium = new TypeError(
    'Failed to fetch dynamically imported module: https://oj.lan/assets/cpp-x1.js?v=3',
  );
  const firefox = new TypeError(
    'error loading dynamically imported module: https://oj.lan/assets/cpp-x1.js',
  );
  assert.equal(
    failedModuleUrl(chromium),
    'https://oj.lan/assets/cpp-x1.js?v=3',
  );
  assert.equal(failedModuleUrl(firefox), 'https://oj.lan/assets/cpp-x1.js');
  assert.equal(
    failedModuleUrl(new TypeError('Importing a module script failed.')),
    null,
  );
  const first = retryUrl('https://oj.lan/assets/cpp-x1.js?v=3');
  const again = retryUrl(first);
  assert.match(first, /^https:\/\/oj\.lan\/assets\/cpp-x1\.js\?v=3&retry=\d+$/);
  assert.match(again, /\?v=3&retry=\d+$/);
  assert.notEqual(first, again);
});

test('a stylesheet that failed to preload is left to a page reload', async () => {
  const css = new Error('Unable to preload CSS for /assets/editor-x1.css');
  assert.ok(isChunkLoadError(css));
  assert.ok(!isRetryableChunkLoadError(css));
  let calls = 0;
  await assert.rejects(
    retryLoad(async () => {
      calls += 1;
      throw css;
    }, harness().options),
  );
  assert.equal(calls, 1);
});
