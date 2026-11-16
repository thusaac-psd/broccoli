import type { SubmissionListPagination } from '@broccoli/web-sdk/submission';
import { Button } from '@broccoli/web-sdk/ui';
import { ChevronLeft, ChevronRight } from 'lucide-react';

/**
 * Previous/next controls for a cursor-paged submission list. The server
 * withholds the total when a visibility plugin may hide matching
 * submissions; the pager then shows the page number alone and follows
 * `has_more`. `page` is `null` when the page number is unknown (a deep link
 * to a later page).
 */
export function SubmissionListPager({
  pagination,
  page,
  hasPrevious,
  onNext,
  onPrevious,
}: {
  pagination: SubmissionListPagination | undefined;
  page: number | null;
  hasPrevious: boolean;
  onNext: (cursor: string) => void;
  onPrevious: () => void;
}) {
  if (!pagination) return null;
  const {
    per_page: perPage,
    total,
    total_pages: totalPages,
    next_cursor: nextCursor,
  } = pagination;
  if (!hasPrevious && !nextCursor) return null;

  return (
    <div className="flex items-center justify-between text-sm text-muted-foreground">
      <span className="tabular-nums">
        {total != null && page != null
          ? `${(page - 1) * perPage + 1}–${Math.min(page * perPage, total)} of ${total}`
          : null}
      </span>
      <div className="flex items-center gap-1">
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={!hasPrevious}
          onClick={onPrevious}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <span className="px-3 text-sm tabular-nums">
          {page != null && totalPages != null
            ? `${page} / ${totalPages}`
            : (page ?? '…')}
        </span>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={!nextCursor}
          onClick={() => nextCursor && onNext(nextCursor)}
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
