// Pure-logic test for `buildAdminJobsQueryParams`, matching the
// `node --experimental-strip-types --test` convention used by
// packages/web/src/**/*.test.ts: no bundler/DOM needed since the function
// under test is pure.
//
// Guards the print queue's station/printer filters: staff picking a station
// in `PrintQueuePage`'s toolbar must see only that station's jobs. A
// hand-maintained key whitelist in `usePrintApi.adminListJobs` would drop
// `station`/`printer` silently -- they compile fine at the `PrintQueuePage`
// call site (an object literal spread into a wider type) -- even though the
// server (`handle_admin_list_jobs` + `build_filter_where` in
// plugins/print/src) filters on both.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { buildAdminJobsQueryParams } from './adminJobsQuery.ts';

test('carries the station and printer filters into the query string', () => {
  const params = buildAdminJobsQueryParams({
    page: 1,
    per_page: 25,
    station: 'front-desk',
    printer: 'hp-laserjet-3',
  });

  assert.equal(params.get('station'), 'front-desk');
  assert.equal(params.get('printer'), 'hp-laserjet-3');
});

test('omits empty-string and undefined optional filters entirely', () => {
  const params = buildAdminJobsQueryParams({
    page: 2,
    per_page: 25,
    status: '',
    station: undefined,
  });

  assert.equal(params.has('status'), false);
  assert.equal(params.has('station'), false);
  assert.equal(params.get('page'), '2');
  assert.equal(params.get('per_page'), '25');
});

test('carries every other filter unchanged', () => {
  const params = buildAdminJobsQueryParams({
    page: 3,
    per_page: 50,
    search: 'alice',
    sort_by: 'created_at',
    sort_order: 'asc',
    status: 'pending_approval',
  });

  assert.equal(
    params.toString(),
    'page=3&per_page=50&search=alice&sort_by=created_at&sort_order=asc&status=pending_approval',
  );
});
