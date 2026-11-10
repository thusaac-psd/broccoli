import assert from 'node:assert/strict';
import { test } from 'node:test';

import { isChunkLoadError } from './chunk-reload.ts';

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
