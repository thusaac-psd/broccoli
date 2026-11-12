import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { ActivePluginManifest, PluginModule } from '../types.ts';
import {
  createPluginRegistry,
  type PluginRegistryDeps,
  registryView,
} from './plugin-loads.ts';

function manifest(id: string): ActivePluginManifest {
  return {
    id,
    name: id,
    entry: `/assets/${id}/index.js`,
    css: [],
    components: { [`${id}.view`]: 'View' },
    routes: [{ path: `/${id}`, component: `${id}.view` }],
    slots: [],
  } as unknown as ActivePluginManifest;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function untilLoading(
  r: ReturnType<typeof createPluginRegistry>,
  id: string,
) {
  while (r.getLoads().get(id)?.status !== 'loading') {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

const moduleFor = (onInit?: () => Promise<void>): PluginModule =>
  ({ View: () => null, onInit }) as unknown as PluginModule;

function registry(overrides: Partial<PluginRegistryDeps> = {}) {
  const imports: Array<{ id: string; fresh: boolean }> = [];
  const removed: string[] = [];
  let i18nRefreshes = 0;
  let active: ActivePluginManifest[] | null = [manifest('a')];
  const r = createPluginRegistry({
    fetchActive: async () => active,
    importRemote: async (m, fresh) => {
      imports.push({ id: m.id, fresh });
      return moduleFor();
    },
    removeRemote: (id) => removed.push(id),
    // Up to three tries, no waiting.
    retry: async (load) => {
      for (let attempt = 0; ; attempt += 1) {
        try {
          return await load(attempt);
        } catch (error) {
          if (attempt === 2) throw error;
        }
      }
    },
    refreshI18n: async () => {
      i18nRefreshes += 1;
    },
    ...overrides,
  });
  return {
    r,
    imports,
    removed,
    i18nRefreshes: () => i18nRefreshes,
    setActive: (next: ActivePluginManifest[] | null) => (active = next),
  };
}

test('a plugin loaded twice at once is fetched and registered once', async () => {
  const { r, imports } = registry();
  await Promise.all([r.sync(), r.sync()]);
  assert.equal(imports.length, 1);
  assert.equal(registryView(r.getLoads()).routes.length, 1);
});

test('a reload during an initializing load registers the plugin once', async () => {
  const init = deferred<void>();
  let first = true;
  const { r } = registry({
    importRemote: async () => {
      const onInit = first ? () => init.promise : undefined;
      first = false;
      return moduleFor(onInit);
    },
  });
  const initial = r.sync();
  await untilLoading(r, 'a');
  const reload = r.reload('a');
  init.resolve();
  await Promise.all([initial, reload]);
  const view = registryView(r.getLoads());
  assert.equal(view.routes.length, 1);
  assert.equal(view.plugins.size, 1);
});

test('a failing onInit is retried, and success clears the error', async () => {
  let inits = 0;
  const { r } = registry({
    importRemote: async () =>
      moduleFor(async () => {
        inits += 1;
        if (inits === 1) throw new Error('config fetch failed');
      }),
  });
  await r.sync();
  const view = registryView(r.getLoads());
  assert.equal(inits, 2);
  assert.equal(view.errors.size, 0);
  assert.equal(view.plugins.size, 1);
});

test('a plugin that keeps failing is failed, not retried forever', async () => {
  const { r, imports, removed } = registry({
    importRemote: async () => {
      throw new TypeError('Failed to fetch dynamically imported module');
    },
  });
  await r.sync();
  await r.sync();
  const view = registryView(r.getLoads());
  assert.equal(view.errors.get('a')?.message.includes('Failed to fetch'), true);
  assert.equal(view.routes.length, 0);
  assert.deepEqual(removed, ['a']);
  // Three tries in the first sync; the second leaves the failure alone.
  assert.equal(imports.length, 0);
});

test('retries and reloads fetch fresh copies', async () => {
  let calls = 0;
  const { r, imports } = registry();
  const failing = registry({
    importRemote: async (_m, fresh) => {
      calls += 1;
      imports.push({ id: 'a', fresh });
      if (calls === 1) throw new Error('blip');
      return moduleFor();
    },
  });
  await failing.r.sync();
  assert.deepEqual(
    imports.map((i) => i.fresh),
    [false, true],
  );
  await r.sync();
  await r.reload('a');
  assert.deepEqual(
    imports.slice(2).map((i) => i.fresh),
    [false, true],
  );
});

test('plugins the server no longer lists are unloaded', async () => {
  const { r, setActive, removed } = registry();
  await r.sync();
  setActive([]);
  await r.sync();
  assert.equal(r.getLoads().size, 0);
  assert.deepEqual(removed, ['a']);
});

test('an unreachable list changes nothing', async () => {
  const { r, setActive } = registry();
  await r.sync();
  setActive(null);
  assert.equal(await r.sync(), false);
  assert.equal(registryView(r.getLoads()).plugins.size, 1);
});

test('a sync refreshes translations once', async () => {
  const { r, setActive, i18nRefreshes } = registry();
  setActive([manifest('a'), manifest('b')]);
  await r.sync();
  assert.equal(i18nRefreshes(), 1);
});

test('a load that settles after its plugin was unloaded is discarded', async () => {
  const imported = deferred<PluginModule>();
  let destroyed = 0;
  const { r } = registry({ importRemote: () => imported.promise });
  const syncing = r.sync();
  await untilLoading(r, 'a');
  await r.unload('a');
  imported.resolve({
    ...moduleFor(),
    onDestroy: async () => {
      destroyed += 1;
    },
  } as PluginModule);
  await syncing;
  assert.equal(r.getLoads().size, 0);
  assert.equal(destroyed, 1);
});

test('a failed onInit is retried on the module already imported', async () => {
  let inits = 0;
  const { r, imports } = registry({
    importRemote: async (m, fresh) => {
      imports.push({ id: m.id, fresh });
      return moduleFor(async () => {
        inits += 1;
        if (inits === 1) throw new Error('config fetch failed');
      });
    },
  });
  await r.sync();
  assert.equal(inits, 2);
  assert.deepEqual(imports, [{ id: 'a', fresh: false }]);
});
