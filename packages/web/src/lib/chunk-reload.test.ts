import assert from 'node:assert/strict';
import { test } from 'node:test';

import { isKnownOffline } from '@broccoli/web-sdk/utils';

import {
  canReloadForFreshChunks,
  type ReachabilityDeps,
  reloadForFreshChunks,
  retryPageLoad,
  waitUntilReachable,
} from './chunk-reload.ts';

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

function reachability(answers: boolean[]) {
  const slept: number[] = [];
  let probes = 0;
  return {
    slept,
    probes: () => probes,
    deps: {
      probe: async () => answers[probes++] ?? true,
      sleep: async (ms: number): Promise<void> => {
        slept.push(ms);
      },
    } as ReachabilityDeps,
  };
}

test('nothing reloads until the server answers, asking again with backoff', async () => {
  const r = reachability([false, false, false, true]);
  assert.equal(
    await waitUntilReachable(new AbortController().signal, r.deps),
    true,
  );
  assert.equal(r.probes(), 4);
  assert.deepEqual(r.slept, [1_000, 2_000, 4_000]);
});

test('a server that answers at once is not waited on', async () => {
  const r = reachability([true]);
  assert.equal(
    await waitUntilReachable(new AbortController().signal, r.deps),
    true,
  );
  assert.deepEqual(r.slept, []);
});

test('waiting stops when the page goes away', async () => {
  const controller = new AbortController();
  const r = reachability([false]);
  r.deps.sleep = async () => controller.abort();
  assert.equal(await waitUntilReachable(controller.signal, r.deps), false);
});
