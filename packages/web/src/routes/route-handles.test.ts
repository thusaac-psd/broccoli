// Every route names its page in the browser tab: each route module exports
// a `handle` (see `lib/route-title.tsx`), and the deepest matched one wins.
// `_app.tsx` is the shell every page renders in, not a page.
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const ROUTES_DIR = dirname(fileURLToPath(import.meta.url));

test('every route module exports a title handle', () => {
  const untitled = readdirSync(ROUTES_DIR)
    .filter((name) => name.endsWith('.tsx') && name !== '_app.tsx')
    .filter(
      (name) =>
        !/^export const handle: RouteHandle = /m.test(
          readFileSync(`${ROUTES_DIR}/${name}`, 'utf8'),
        ),
    );
  assert.deepEqual(untitled, []);
});
