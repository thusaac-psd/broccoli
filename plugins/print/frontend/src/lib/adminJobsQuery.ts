import type { AdminJobsQuery } from '../types';

/**
 * Serializes an `AdminJobsQuery` into URL query parameters.
 *
 * Iterates every key of `q` generically instead of a hand-maintained
 * per-field whitelist. A previous whitelist-style implementation in
 * `usePrintApi.adminListJobs` explicitly set exactly six keys
 * (`page`/`per_page`/`search`/`sort_by`/`sort_order`/`status`); when
 * `station` and `printer` were later added to `AdminJobsQuery` and passed
 * at the `PrintQueuePage` call site, they compiled fine (the object literal
 * was spread, not the whitelist) but were silently dropped here, so the
 * station/printer filters never reached the server even though
 * `handle_admin_list_jobs` and `build_filter_where` (plugins/print/src)
 * already supported them. Adding a field to `AdminJobsQuery` now flows
 * through automatically -- there is no second place to remember.
 */
export function buildAdminJobsQueryParams(q: AdminJobsQuery): URLSearchParams {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(q)) {
    if (value === undefined || value === '') continue;
    params.set(key, String(value));
  }
  return params;
}
