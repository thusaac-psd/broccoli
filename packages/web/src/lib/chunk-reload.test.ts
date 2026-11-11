import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  canReloadForFreshChunks,
  isChunkLoadError,
  isKnownOffline,
  reloadForFreshChunks,
  retryPageLoad,
} from './chunk-reload.ts';

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

function withBrowser(onLine: boolean, run: (reloads: () => number) => void) {
  const g = globalThis as Record<string, unknown>;
  const saved = ['navigator', 'sessionStorage', 'window'].map(
    (key) => [key, Object.getOwnPropertyDescriptor(g, key)] as const,
  );
  const items = new Map<string, string>();
  let reloads = 0;
  const define = (key: string, value: unknown) =>
    Object.defineProperty(g, key, {
      value,
      configurable: true,
      writable: true,
    });
  define('navigator', { onLine });
  define('sessionStorage', {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
  });
  define('window', {
    location: {
      reload: () => {
        reloads += 1;
      },
    },
  });
  try {
    run(() => reloads);
  } finally {
    for (const [key, descriptor] of saved) {
      if (descriptor) Object.defineProperty(g, key, descriptor);
      else delete g[key];
    }
  }
}

test('a chunk-load failure reloads once', () => {
  withBrowser(true, (reloads) => {
    assert.equal(canReloadForFreshChunks(), true);
    assert.equal(reloadForFreshChunks(), true);
    assert.equal(reloads(), 1);
    // The reload did not help: no loop.
    assert.equal(reloadForFreshChunks(), false);
    assert.equal(reloads(), 1);
  });
});

test('offline, nothing reloads onto the browser offline page', () => {
  withBrowser(false, (reloads) => {
    assert.equal(isKnownOffline(), true);
    assert.equal(canReloadForFreshChunks(), false);
    assert.equal(reloadForFreshChunks(), false);
    assert.equal(retryPageLoad(), false);
    assert.equal(reloads(), 0);
  });
  // Back online, the error page's retry and the deferred reload both go ahead.
  withBrowser(true, (reloads) => {
    assert.equal(isKnownOffline(), false);
    assert.equal(retryPageLoad(), true);
    assert.equal(reloadForFreshChunks(), true);
    assert.equal(reloads(), 2);
  });
});
