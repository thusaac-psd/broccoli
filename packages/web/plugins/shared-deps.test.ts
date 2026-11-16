import assert from 'node:assert/strict';
import { test } from 'node:test';

import { sharedDepsPlugin } from './shared-deps.ts';

const PLACEHOLDER = '__BROCCOLI_SHARED_DEPS_MAP__';

// Drives the hooks the way a client production build does, with just enough
// of the plugin context for the paths under test.
function clientBuild() {
  const plugin = sharedDepsPlugin() as unknown as {
    config(user: unknown, env: { command: string }): void;
    configResolved(config: unknown): void;
    renderChunk(
      this: unknown,
      code: string,
      chunk: { fileName: string; moduleIds: string[] },
    ): { code: string } | null;
    generateBundle(this: unknown, options: unknown, bundle: unknown): void;
  };
  plugin.config({}, { command: 'build' });
  plugin.configResolved({ base: '/', build: { ssr: false } });
  const emitted: string[] = [];
  const context = {
    error(message: string): never {
      throw new Error(message);
    },
    getFileName: () => 'assets/unused.js',
    emitFile(file: { fileName: string }) {
      emitted.push(file.fileName);
      return file.fileName;
    },
  };
  return { plugin, context, emitted };
}

const chunk = (fileName: string, code: string) => ({
  type: 'chunk' as const,
  fileName,
  code,
  imports: [],
  moduleIds: [],
});

test('the map is filled in even if the virtual module id is spelled differently', () => {
  const { plugin, context } = clientBuild();
  const rendered = plugin.renderChunk.call(
    context,
    `const m = ${PLACEHOLDER};\nexport { m };`,
    { fileName: 'assets/root-x.js', moduleIds: ['virtual:shared-deps-map'] },
  );
  assert.ok(rendered);
  assert.ok(!rendered.code.includes(PLACEHOLDER));
  assert.match(rendered.code, /const m = \{\};/);
});

test('a bundle that still holds the placeholder fails the build', () => {
  const { plugin, context, emitted } = clientBuild();
  assert.throws(
    () =>
      plugin.generateBundle.call(
        context,
        {},
        {
          'assets/entry-a.js': chunk('assets/entry-a.js', 'export {};'),
          'assets/root-x.js': chunk(
            'assets/root-x.js',
            `const m = ${PLACEHOLDER};`,
          ),
        },
      ),
    /left unreplaced in assets\/root-x\.js/,
  );
  assert.deepEqual(emitted, []);
});

test('a bundle without the placeholder builds', () => {
  const { plugin, context, emitted } = clientBuild();
  plugin.generateBundle.call(
    context,
    {},
    { 'assets/root-x.js': chunk('assets/root-x.js', 'const m = {};') },
  );
  assert.deepEqual(emitted, ['shared-deps-map.json']);
});

// A plugin bundle reaches a shared dep through the import map, the host
// through its own imports. In dev both must load one module instance, so the
// shim the map points at must import exactly the URL a host module gets.
test('in dev a shared dep resolves to the module the host imports', async () => {
  const { mkdtempSync, rmSync, writeFileSync } = await import('node:fs');
  const { join } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const { createServer } = await import('vite');
  const { devImportMap, resolveSdkEntry } = await import('./shared-deps.ts');

  const root = fileURLToPath(new URL('..', import.meta.url));
  const dir = mkdtempSync(join(root, 'node_modules', '.shared-deps-test-'));
  const server = await createServer({
    configFile: false,
    root,
    logLevel: 'silent',
    appType: 'custom',
    server: { middlewareMode: true, hmr: false, ws: false },
    optimizeDeps: { noDiscovery: true, include: [] },
    resolve: {
      alias: [
        {
          find: '@broccoli/web-sdk/ui',
          replacement: resolveSdkEntry('@broccoli/web-sdk/ui'),
        },
      ],
    },
    plugins: [sharedDepsPlugin()],
  });
  const { createServer: createHttpServer } = await import('node:http');
  const http = createHttpServer(server.middlewares);
  await new Promise<void>((done) => http.listen(0, '127.0.0.1', done));
  const { port } = http.address() as { port: number };
  try {
    // What the browser gets for `url`, and the URL its first import names.
    const importedUrl = async (url: string) => {
      const response = await fetch(`http://127.0.0.1:${port}${url}`);
      assert.equal(response.status, 200, url);
      const match = /from\s*["']([^"']+)["']/.exec(await response.text());
      assert.ok(match, `${url} imports its dep`);
      return match[1];
    };
    for (const dep of ['react-router', '@broccoli/web-sdk/ui']) {
      const host = join(dir, `${dep.replace(/\W/g, '_')}.ts`);
      writeFileSync(host, `export * from '${dep}';\n`);
      assert.equal(
        await importedUrl(devImportMap()[dep]),
        await importedUrl(`/@fs${host}`),
        dep,
      );
    }
  } finally {
    http.close();
    await server.close();
    rmSync(dir, { recursive: true, force: true });
  }
});
