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
