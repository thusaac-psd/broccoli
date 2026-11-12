import type { SubmissionListPagination } from '@broccoli/web-sdk/submission';
import { Button } from '@broccoli/web-sdk/ui';
import { ChevronLeft, ChevronRight } from 'lucide-react';

/**
 * Previous/next controls for a submission list. The server withholds the
 * total when a visibility plugin may hide matching submissions; the pager
 * then shows the page number alone and follows `has_more`.
 */
export function SubmissionListPager({
  pagination,
  onPageChange,
}: {
  pagination: SubmissionListPagination | undefined;
  onPageChange: (page: number) => void;
}) {
  if (!pagination) return null;
  const {
    page,
    per_page: perPage,
    total,
    total_pages: totalPages,
  } = pagination;
  if (page <= 1 && !pagination.has_more) return null;

  return (
    <div className="flex items-center justify-between text-sm text-muted-foreground">
      <span className="tabular-nums">
        {total != null
          ? `${(page - 1) * perPage + 1}–${Math.min(page * perPage, total)} of ${total}`
          : null}
      </span>
      <div className="flex items-center gap-1">
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={page <= 1}
          onClick={() => onPageChange(page - 1)}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <span className="px-3 text-sm tabular-nums">
          {totalPages != null ? `${page} / ${totalPages}` : page}
        </span>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={!pagination.has_more}
          onClick={() => onPageChange(page + 1)}
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
