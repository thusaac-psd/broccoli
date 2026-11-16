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
import { AlertCircle, Check, FileText, Loader2, Printer } from 'lucide-react';
import { useMemo, useState } from 'react';

import { ApiError, usePrintApi } from './hooks/usePrintApi';
import { estimatePages } from './lib/format';

interface Props {
  submission?: Submission | null;
}

type Phase = 'idle' | 'sending' | 'done' | 'error';

export function PrintSubmissionButton({ submission }: Props) {
  const { t } = useTranslation();
  const api = usePrintApi();
  const [open, setOpen] = useState(false);
  const [phase, setPhase] = useState<Phase>('idle');
  const [message, setMessage] = useState<string | null>(null);

  const files = submission?.files ?? [];
  const totalPages = useMemo(
    () => files.reduce((acc, f) => acc + estimatePages(f.content), 0),
    [files],
  );

  // Only meaningful once a submission with source exists.
  if (!submission || files.length === 0) return null;

  const reset = () => {
    setPhase('idle');
    setMessage(null);
  };

  const submit = async () => {
    setPhase('sending');
    setMessage(null);
    try {
      let resultStatus: string | undefined;
      if (submission.contest_id != null) {
        const r = await api.printSubmission(
          submission.contest_id,
          submission.id,
        );
        resultStatus = r.status;
      } else {
        // Without a contest, print each file as standalone text.
        for (const f of files) {
          const r = await api.printArbitrary({
            filename: f.filename,
            language: submission.language,
            source: f.content,
          });
          resultStatus = r.status;
        }
      }
      setPhase('done');
      setMessage(
        resultStatus === 'pending_approval'
          ? t('print.action.needsApproval')
          : t('print.action.submitted'),
      );
      window.setTimeout(() => {
        setOpen(false);
        reset();
      }, 2000);
    } catch (e) {
      setPhase('error');
      setMessage(e instanceof ApiError ? e.message : t('print.action.failed'));
    }
  };

  return (
    <>
      <div className="flex justify-end">
        <Button
          variant="outline"
          size="sm"
          className="gap-1.5"
          onClick={() => {
            reset();
            setOpen(true);
          }}
        >
          <Printer className="h-4 w-4" />
          {t('print.action.print')}
        </Button>
      </div>

      <Dialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) reset();
        }}
      >
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
                <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
                  ~{estimatePages(f.content)}p
                </span>
              </div>
            ))}
          </div>

          <p className="text-xs text-muted-foreground">
            {t('print.submission.files', {
              count: files.length,
              pages: totalPages,
            })}
          </p>

          {phase === 'error' && message && (
            <p className="flex items-center gap-2 text-sm text-red-600 dark:text-red-400">
              <AlertCircle className="h-4 w-4 shrink-0" />
              {message}
            </p>
          )}
          {phase === 'done' && message && (
            <p className="flex items-center gap-2 text-sm text-emerald-600 dark:text-emerald-400">
              <Check className="h-4 w-4 shrink-0" />
              {message}
            </p>
          )}

          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => setOpen(false)}
              disabled={phase === 'sending'}
            >
              {t('print.dialog.cancel')}
            </Button>
            <Button
              onClick={submit}
              disabled={phase === 'sending' || phase === 'done'}
              className="gap-1.5 min-w-28"
            >
              {phase === 'sending' && (
                <Loader2 className="h-4 w-4 animate-spin" />
              )}
              {phase === 'done' && <Check className="h-4 w-4" />}
              {phase === 'sending'
                ? t('print.action.printing')
                : phase === 'done'
                  ? t('print.action.submitted')
                  : t('print.dialog.submit')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
