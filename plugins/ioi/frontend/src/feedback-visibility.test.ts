import assert from 'node:assert/strict';
import { test } from 'node:test';

import { hostShowsResult } from './feedback-visibility.ts';

test('the host shows a result the plugin has nothing to add to', () => {
  // Withheld at feedback "none": no status, no result, and the host says why.
  assert.equal(
    hostShowsResult({ status: null, withheld: { reason: 'hidden' } }),
    true,
  );
  assert.equal(hostShowsResult({ status: 'CompilationError' }), true);
  assert.equal(hostShowsResult({ status: 'Judged', withheld: null }), false);
});
