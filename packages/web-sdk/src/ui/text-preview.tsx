import { useQuery } from '@tanstack/react-query';
import { useId, useState } from 'react';

import { useTranslation } from '@/i18n';
import { Button } from '@/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/ui/dialog';

const PREVIEW_CHARS = 200;
const PREVIEW_LINES = 5;

/** Keep multiline output compact and load larger text only when the dialog opens. */
export function TextPreview({
  label,
  text,
  loadText,
}: {
  label: string;
  text: string;
  loadText?: (signal?: AbortSignal) => Promise<string>;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const cacheKey = useId();
  const preview = text
    .slice(0, PREVIEW_CHARS)
    .split('\n')
    .slice(0, PREVIEW_LINES)
    .join('\n');
  const truncated =
    preview.length < text.length ||
    text.includes('… (truncated)') ||
    text.includes('... (truncated)');
  return (
    <div className="min-w-0 space-y-1">
      <div className="flex items-center gap-2">
        <span className="text-xs font-medium text-muted-foreground">
          {label}
        </span>
        {(truncated || loadText) && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-auto px-2 py-0.5 text-xs"
            onClick={() => setOpen(true)}
          >
            {t('result.expandOutput')}
          </Button>
        )}
      </div>
      {/* Scrolls when long: reachable by keyboard. */}
      <pre
        tabIndex={0}
        aria-label={label}
        className="max-h-28 max-w-full overflow-auto whitespace-pre rounded bg-muted p-2 text-xs"
      >
        {preview}
        {truncated && '…'}
      </pre>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="flex max-h-[85vh] min-h-0 min-w-0 flex-col sm:max-w-4xl">
          <DialogHeader>
            <DialogTitle>{label}</DialogTitle>
            <DialogDescription>
              {t('result.outputDialogHint')}
            </DialogDescription>
          </DialogHeader>
          {open && (
            <OutputDialogBody
              text={text}
              loadText={loadText}
              cacheKey={cacheKey}
            />
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}

function OutputDialogBody({
  text,
  loadText,
  cacheKey,
}: {
  text: string;
  loadText?: (signal?: AbortSignal) => Promise<string>;
  cacheKey: string;
}) {
  const { t } = useTranslation();
  const query = useQuery({
    queryKey: ['text-preview', cacheKey],
    queryFn: ({ signal }) =>
      loadText ? loadText(signal) : Promise.resolve(text),
    enabled: true,
    staleTime: Infinity,
    gcTime: 0,
  });
  return (
    <>
      {query.isLoading ? (
        <p className="text-sm text-muted-foreground">
          {t('result.loadingOutput')}
        </p>
      ) : query.isError ? (
        <div className="space-y-2">
          <p className="text-sm text-destructive">
            {t('result.outputLoadError')}
          </p>
          <Button
            type="button"
            variant="outline"
            onClick={() => query.refetch()}
          >
            {t('result.retryOutput')}
          </Button>
        </div>
      ) : (
        <pre className="min-h-0 min-w-0 max-h-[65vh] max-w-full overflow-auto whitespace-pre rounded bg-muted p-3 text-xs">
          {query.data}
        </pre>
      )}
    </>
  );
}
