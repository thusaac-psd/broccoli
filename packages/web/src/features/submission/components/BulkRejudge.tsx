import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { Loader2, RotateCcw } from 'lucide-react';

import type { BulkRejudge } from '@/features/submission/hooks/use-bulk-rejudge';

/** Turns bulk mode on and off; nothing for a viewer who may not rejudge. */
export function BulkRejudgeToggle({ bulk }: { bulk: BulkRejudge }) {
  const { t } = useTranslation();
  if (!bulk.allowed) return null;
  return (
    <Button
      className="h-9 shrink-0"
      variant={bulk.active ? 'secondary' : 'outline'}
      onClick={bulk.toggle}
    >
      <RotateCcw className="mr-1 h-4 w-4" />
      {bulk.active
        ? t('submissions.bulkRejudge.exitMode')
        : t('submissions.bulkRejudge.enterMode')}
    </Button>
  );
}

/** The selection tools and the rejudge action, while bulk mode is on. */
export function BulkRejudgeBar({ bulk }: { bulk: BulkRejudge }) {
  const { t } = useTranslation();
  if (!bulk.active) return null;
  const noMatches = bulk.matchingLoading || bulk.matchingIds.length === 0;
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed px-3 py-2">
      <Button
        variant="outline"
        className="h-8"
        disabled={noMatches}
        onClick={bulk.selectMatching}
      >
        {t('submissions.selection.selectMatched')}
      </Button>
      <Button
        variant="outline"
        className="h-8"
        disabled={noMatches}
        onClick={bulk.unselectMatching}
      >
        {t('submissions.selection.unselectMatched')}
      </Button>
      <Button
        variant="ghost"
        className="h-8"
        disabled={bulk.selected.size === 0}
        onClick={bulk.clear}
      >
        {t('submissions.selection.clear')}
      </Button>
      <span className="text-xs text-muted-foreground tabular-nums">
        {t('submissions.selection.selectedCount', {
          count: bulk.selected.size,
        })}{' '}
        ·{' '}
        {t('submissions.selection.matchingCount', {
          count: bulk.matchingIds.length,
        })}
      </span>
      <label className="inline-flex items-center gap-2 text-xs text-muted-foreground">
        <input
          type="checkbox"
          checked={bulk.applyImmediately}
          onChange={(event) =>
            bulk.setApplyImmediately(event.currentTarget.checked)
          }
          className="h-4 w-4 rounded border-muted"
        />
        {t('submissions.bulkRejudge.applyImmediately')}
      </label>
      <Button
        className="h-8 ml-auto"
        disabled={bulk.pending || bulk.selected.size === 0}
        onClick={bulk.submit}
      >
        {bulk.pending ? (
          <Loader2 className="mr-1 h-4 w-4 animate-spin" />
        ) : (
          <RotateCcw className="mr-1 h-4 w-4" />
        )}
        {t('submissions.bulkRejudge.action')}
      </Button>
    </div>
  );
}
