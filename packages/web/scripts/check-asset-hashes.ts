/**
 * Checks that every content-hashed file name in `build/client/assets` pins its
 * bytes: a rebuild may add, remove or rename files, but it must never ship
 * different bytes under a name the previous build already used. Long-lived
 * (`immutable`) caching of `/assets/*` depends on this, and so does the
 * shared-deps import map, which must keep plugins loading after a redeploy.
 *
 * It builds the web app, then rebuilds it after each of three edits that reach
 * the app mainly through shared-deps shims:
 *   - a new web-sdk subpath (adds a shim, so only the import map baked into
 *     the root chunk changes; the host's own imports stay the same),
 *   - a new export in an existing web-sdk subpath (changes a shim chunk), and
 *   - a new stylesheet under the Monaco setup module (changes the CSS list the
 *     `@monaco-editor/react` shim links itself).
 *
 * After every build it also checks that `build/client/index.html` carries an
 * import map with every shared dep, each pointing at a file that exists.
 *
 * Every edit is reverted afterwards and web-sdk and the web app are rebuilt.
 *
 * Run from the repo root (takes a couple of minutes; set VERBOSE=1 to list
 * the new file names of each scenario):
 *   node --experimental-strip-types packages/web/scripts/check-asset-hashes.ts
 * or, as the CI frontend job does, run the web package's
 * `check:asset-hashes` script.
 */
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { SHARED_DEPS } from '../plugins/shared-deps.ts';

const WEB_ROOT = fileURLToPath(new URL('..', import.meta.url));
const SDK_ROOT = resolve(WEB_ROOT, '../web-sdk');
const CLIENT_DIR = resolve(WEB_ROOT, 'build/client');
const ASSETS_DIR = resolve(CLIENT_DIR, 'assets');

const SDK_PROBE_FILE = resolve(SDK_ROOT, 'src/utils/index.ts');
const SDK_PROBE_SUBPATH = resolve(SDK_ROOT, 'src/__asset-hash-probe');
const MONACO_SETUP_FILE = resolve(WEB_ROOT, 'src/lib/monaco-setup.ts');
const CSS_PROBE_FILE = resolve(WEB_ROOT, 'src/lib/__asset-hash-probe.css');

type Snapshot = Map<string, string>;

interface Scenario {
  name: string;
  touchesSdk: boolean;
  apply(nonce: string): void;
  revert(): void;
}

function run(cwd: string, args: string[]): void {
  execFileSync('pnpm', args, { cwd, stdio: ['ignore', 'ignore', 'inherit'] });
}

function buildSdk(): void {
  run(SDK_ROOT, ['build']);
}

function buildWeb(): void {
  run(WEB_ROOT, ['build']);
}

function snapshot(): Snapshot {
  const files: Snapshot = new Map();
  for (const name of readdirSync(ASSETS_DIR)) {
    const bytes = readFileSync(resolve(ASSETS_DIR, name));
    files.set(name, createHash('sha256').update(bytes).digest('hex'));
  }
  return files;
}

/** Problems with the import map in the built index.html, if any. */
function checkImportMap(): string[] {
  const html = readFileSync(resolve(CLIENT_DIR, 'index.html'), 'utf-8');
  const match =
    /<script\b[^>]*\btype=["']importmap["'][^>]*>([\s\S]*?)<\/script>/i.exec(
      html,
    );
  if (!match) return ['index.html has no import map'];

  const { imports } = JSON.parse(match[1]) as {
    imports: Record<string, string>;
  };
  const problems: string[] = [];
  for (const dep of SHARED_DEPS) {
    const url = imports[dep];
    if (!url) {
      problems.push(`import map is missing ${dep}`);
    } else if (!existsSync(resolve(CLIENT_DIR, url.replace(/^\//, '')))) {
      problems.push(`import map points ${dep} at missing ${url}`);
    }
  }
  return problems;
}

function snapshotChecked(label: string): Snapshot {
  const problems = checkImportMap();
  if (problems.length > 0) {
    throw new Error(`${label}: ${problems.join('; ')}`);
  }
  return snapshot();
}

function appendTemporarily(file: string, text: string): () => void {
  const original = readFileSync(file, 'utf-8');
  writeFileSync(file, original + text);
  return () => writeFileSync(file, original);
}

function makeScenarios(): Scenario[] {
  let undoSdk: (() => void) | undefined;
  let undoSetup: (() => void) | undefined;

  return [
    {
      name: 'new web-sdk subpath',
      touchesSdk: true,
      apply(nonce) {
        mkdirSync(SDK_PROBE_SUBPATH);
        writeFileSync(
          resolve(SDK_PROBE_SUBPATH, 'index.ts'),
          `export const assetHashProbe = '${nonce}';\n`,
        );
      },
      revert() {
        rmSync(SDK_PROBE_SUBPATH, { recursive: true, force: true });
      },
    },
    {
      name: 'web-sdk subpath export',
      touchesSdk: true,
      apply(nonce) {
        undoSdk = appendTemporarily(
          SDK_PROBE_FILE,
          `\nexport const __assetHashProbe = '${nonce}';\n`,
        );
      },
      revert() {
        undoSdk?.();
        undoSdk = undefined;
      },
    },
    {
      name: 'Monaco shim stylesheet',
      touchesSdk: false,
      apply(nonce) {
        writeFileSync(
          CSS_PROBE_FILE,
          `.asset-hash-probe-${nonce} { color: red; }\n`,
        );
        undoSetup = appendTemporarily(
          MONACO_SETUP_FILE,
          `\nimport './__asset-hash-probe.css';\n`,
        );
      },
      revert() {
        undoSetup?.();
        undoSetup = undefined;
        rmSync(CSS_PROBE_FILE, { force: true });
      },
    },
  ];
}

/** Names whose bytes differ between the two builds. */
function compare(before: Snapshot, after: Snapshot): string[] {
  const reused: string[] = [];
  for (const [name, hash] of after) {
    const previous = before.get(name);
    if (previous !== undefined && previous !== hash) reused.push(name);
  }
  return reused.sort();
}

function main(): number {
  for (const leftover of [CSS_PROBE_FILE, SDK_PROBE_SUBPATH]) {
    if (existsSync(leftover)) {
      console.error(`Leftover probe from an earlier run: ${leftover}`);
      return 1;
    }
  }

  console.log('Building baseline...');
  buildSdk();
  buildWeb();
  const baseline = snapshotChecked('baseline');

  let failed = false;
  for (const scenario of makeScenarios()) {
    const nonce = Date.now().toString(36);
    console.log(`\nScenario: ${scenario.name}`);
    let after: Snapshot;
    try {
      scenario.apply(nonce);
      if (scenario.touchesSdk) buildSdk();
      buildWeb();
      after = snapshotChecked(scenario.name);
    } finally {
      scenario.revert();
    }

    const added = [...after.keys()].filter((name) => !baseline.has(name));
    const reused = compare(baseline, after);
    console.log(`  ${added.length} new file names`);
    if (process.env.VERBOSE)
      added.forEach((name) => console.log(`    ${name}`));

    if (added.length === 0) {
      console.error('  FAIL: the edit changed no output, so it proves nothing');
      failed = true;
    } else if (reused.length > 0) {
      console.error(
        `  FAIL: ${reused.length} file(s) kept their name with new bytes:`,
      );
      for (const name of reused) console.error(`    ${name}`);
      failed = true;
    } else {
      console.log('  OK: every changed file got a new name');
    }
  }

  console.log('\nRestoring web-sdk and web builds...');
  buildSdk();
  buildWeb();

  return failed ? 1 : 0;
}

process.exitCode = main();
