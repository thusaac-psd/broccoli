import assert from 'node:assert/strict';
import {
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { brotliDecompressSync, gunzipSync } from 'node:zlib';

import config from '../react-router.config.ts';
import { compressedVariants } from './precompress.ts';

test('compressible assets get brotli and gzip copies of the same bytes', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'precompress-'));
  try {
    const script = 'export const greeting = "hello";\n'.repeat(200);
    writeFileSync(join(dir, 'app-abc123.js'), script);
    writeFileSync(join(dir, 'tiny-abc123.js'), 'export {};\n');
    writeFileSync(join(dir, 'logo-abc123.png'), Buffer.alloc(4096));

    const variants = new Map(await compressedVariants(dir));
    assert.deepEqual(
      [...variants.keys()].map((path) => path.slice(dir.length + 1)).sort(),
      ['app-abc123.js.br', 'app-abc123.js.gz'],
    );
    const br = variants.get(join(dir, 'app-abc123.js.br'))!;
    const gz = variants.get(join(dir, 'app-abc123.js.gz'))!;
    assert.equal(brotliDecompressSync(br).toString(), script);
    assert.equal(gunzipSync(gz).toString(), script);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the build compresses every asset once React Router has written its own', async () => {
  const root = mkdtempSync(join(tmpdir(), 'precompress-build-'));
  try {
    const assets = join(root, 'client', 'assets');
    mkdirSync(assets, { recursive: true });
    const manifest = `window.__reactRouterManifest=${JSON.stringify({
      routes: 'x'.repeat(4096),
    })};`;
    // React Router writes its browser manifest after Vite's bundle closes.
    writeFileSync(join(assets, 'manifest-0123abcd.js'), manifest);
    writeFileSync(join(assets, 'app-abc123.css'), 'a{color:red}\n'.repeat(200));

    await config.buildEnd!({
      buildManifest: undefined,
      reactRouterConfig: { buildDirectory: root },
      viteConfig: { build: { assetsDir: 'assets' } },
    } as unknown as Parameters<NonNullable<typeof config.buildEnd>>[0]);

    const written = readdirSync(assets).sort();
    for (const asset of ['manifest-0123abcd.js', 'app-abc123.css']) {
      assert.ok(written.includes(`${asset}.br`), `${asset} has no .br`);
      assert.ok(written.includes(`${asset}.gz`), `${asset} has no .gz`);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
