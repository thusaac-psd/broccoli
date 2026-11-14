import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { brotliDecompressSync, gunzipSync } from 'node:zlib';

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
