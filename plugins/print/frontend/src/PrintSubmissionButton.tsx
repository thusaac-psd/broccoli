import { useErrorMessage } from '@broccoli/web-sdk/api';
import { useAsyncDialog } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { Submission } from '@broccoli/web-sdk/submission';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';
import { AlertCircle, FileText, Loader2, Printer } from 'lucide-react';

import { usePrintApi } from './hooks/usePrintApi';
import { announcePrinted, printFailure } from './lib/send';

interface Props {
  submission?: Submission | null;
}

export function PrintSubmissionButton({ submission }: Props) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const api = usePrintApi();
  const dialog = useAsyncDialog();

  const files = submission?.files ?? [];

  // Only a contest submission with source can be printed.
  if (!submission || submission.contest_id == null || files.length === 0) {
    return null;
  }
  const contestId = submission.contest_id;

  const submit = () =>
    void dialog.submit(async () => {
      announcePrinted(await api.printSubmission(contestId, submission.id), t);
    });

  return (
    <>
      <div className="flex justify-end">
        <Button
          variant="outline"
          size="sm"
          className="gap-1.5"
          onClick={() => dialog.onOpenChange(true)}
        >
          <Printer className="h-4 w-4" />
          {t('print.action.print')}
        </Button>
      </div>

      <Dialog open={dialog.open} onOpenChange={dialog.onOpenChange}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Printer className="h-4.5 w-4.5 text-primary" />
              {t('print.submission.title')}
            </DialogTitle>
            <DialogDescription>
              {t('print.dialog.description')}
            </DialogDescription>
          </DialogHeader>

          <div className="rounded-lg border border-border bg-muted/40 divide-y divide-border">
            {files.map((f) => (
              <div
                key={f.filename}
                className="flex items-center justify-between gap-3 px-3 py-2"
              >
                <span className="flex items-center gap-2 min-w-0">
                  <FileText className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <code className="truncate font-mono text-xs text-foreground">
                    {f.filename}
                  </code>
                </span>
              </div>
            ))}
          </div>

          {dialog.state.status === 'failed' && (
            <p className="flex items-center gap-2 text-sm text-red-600 dark:text-red-400">
              <AlertCircle className="h-4 w-4 shrink-0" />
              {printFailure(dialog.state.error, t, errorMessage)}
            </p>
          )}

          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => dialog.onOpenChange(false)}
              disabled={dialog.submitting}
            >
              {t('print.dialog.cancel')}
            </Button>
            <Button
              onClick={submit}
              disabled={dialog.submitting}
              className="gap-1.5 min-w-28"
            >
              {dialog.submitting && (
                <Loader2 className="h-4 w-4 animate-spin" />
              )}
              {dialog.submitting
                ? t('print.action.printing')
                : t('print.dialog.submit')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
