import assert from 'node:assert/strict';
import { test } from 'node:test';

import { viewerCacheKey } from '../auth/viewer-cache-key.ts';
import { isProblemContentQuery } from './content-cache.ts';

test('viewer keys isolate accounts and permission changes without depending on permission order', () => {
  assert.equal(viewerCacheKey(null), 'guest');
  assert.equal(
    viewerCacheKey({ id: 1, permissions: ['b', 'a'] }),
    viewerCacheKey({ id: 1, permissions: ['a', 'b'] }),
  );
  assert.notEqual(
    viewerCacheKey({ id: 1, permissions: [] }),
    viewerCacheKey({ id: 2, permissions: [] }),
  );
  assert.notEqual(
    viewerCacheKey({ id: 1, permissions: [] }),
    viewerCacheKey({ id: 1, permissions: ['submission:view_all'] }),
  );
});

test('phase cleanup finds both contest and standalone statement/sample caches for the same viewer', () => {
  for (const kind of ['problem', 'problem-sample-contents']) {
    for (const contest of [7, null]) {
      assert.equal(
        isProblemContentQuery([kind, 101, contest, '10:'], 10, new Set([101])),
        true,
      );
    }
  }
  assert.equal(isProblemContentQuery(['problem', 101, 7, '100:'], 10), false);
  assert.equal(
    isProblemContentQuery(['problem', 102, 7, '10:'], 10, new Set([101])),
    false,
  );
  assert.equal(
    isProblemContentQuery(['editor-draft', 101, 7, '10:'], 10),
    false,
  );
});
