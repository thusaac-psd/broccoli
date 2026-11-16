import type { SubmissionSummary } from '@broccoli/web-sdk/submission';

import type { SubmissionListPage } from '@/features/submission/api/fetch-submissions';

/** The largest page the submission lists serve. */
export const MAX_PER_PAGE = 100;

/** Every row of a cursor-paged submission list, page by page. */
export async function fetchAllPages(
  fetchPage: (cursor: string | null) => Promise<SubmissionListPage>,
): Promise<SubmissionSummary[]> {
  const all: SubmissionSummary[] = [];
  let cursor: string | null = null;
  for (;;) {
    const page = await fetchPage(cursor);
    all.push(...page.data);
    cursor = page.pagination.next_cursor ?? null;
    if (!cursor) return all;
  }
}
