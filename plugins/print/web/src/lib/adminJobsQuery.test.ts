// Pure-logic test for `buildAdminJobsQueryParams`, matching the
// `node --experimental-strip-types --test` convention used by
// packages/web/src/**/*.test.ts: no bundler/DOM needed since the function
// under test is pure.
//
// Regression guard for the print queue's station/printer filters: staff
// picking a station in `PrintQueuePage`'s toolbar used to see every
// station's jobs anyway, because `usePrintApi.adminListJobs` built its
// query string from a hand-maintained whitelist of exactly six keys that
// never included `station`/`printer` -- the fields compiled fine at the
// `PrintQueuePage` call site (an object literal spread into a wider type)
// but were silently dropped before the request ever left the browser, even
// though the server (`handle_admin_list_jobs` + `build_filter_where` in
// plugins/print/src) already filters on both.
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

test('still carries every pre-existing filter unchanged', () => {
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
