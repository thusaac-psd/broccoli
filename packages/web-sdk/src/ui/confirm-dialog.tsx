import { type ReactNode, useCallback, useState } from 'react';

import { getErrorMessage } from '@/api/errors';
import { useAsyncSubmit } from '@/hooks/use-async-submit';
import { useTranslation } from '@/i18n';
import { Button } from '@/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/ui/dialog';

export interface ConfirmOptions {
  title: string;
  description?: ReactNode;
  /** Label of the confirm button. Defaults to "Confirm". */
  confirmLabel?: string;
  /** Style the confirm button as destructive (deletions, revocations). */
  destructive?: boolean;
  /**
   * Runs when the user confirms. The dialog stays open while it runs and
   * closes once it resolves; when it rejects, the dialog stays open and shows
   * the error's message (or `failureMessage` when it has none).
   */
  onConfirm: () => Promise<unknown> | unknown;
  failureMessage?: string;
}

interface Request {
  id: number;
  open: boolean;
  options: ConfirmOptions;
}

/**
 * Ask the user to confirm an action in the app's dialog.
 *
 * Returns `confirm(options)`, which opens the dialog, and the dialog element,
 * which the caller renders once:
 *
 * ```tsx
 * const [confirm, confirmDialog] = useConfirmDialog();
 * // ...
 * confirm({ title: t('admin.deleteConfirm'), destructive: true, onConfirm: remove });
 * return <>{rows}{confirmDialog}</>;
 * ```
 */
export function useConfirmDialog(): [
  (options: ConfirmOptions) => void,
  ReactNode,
] {
  const [request, setRequest] = useState<Request | null>(null);

  const confirm = useCallback((options: ConfirmOptions) => {
    setRequest((prev) => ({ id: (prev?.id ?? 0) + 1, open: true, options }));
  }, []);

  const close = useCallback(() => {
    setRequest((prev) => (prev ? { ...prev, open: false } : prev));
  }, []);

  const dialog = request ? (
    <ConfirmDialog
      key={request.id}
      open={request.open}
      options={request.options}
      onClose={close}
    />
  ) : null;

  return [confirm, dialog];
}

function ConfirmDialog({
  open,
  options,
  onClose,
}: {
  open: boolean;
  options: ConfirmOptions;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { state, submitting, submit } = useAsyncSubmit();

  const handleConfirm = async () => {
    if (await submit(async () => options.onConfirm())) onClose();
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !submitting) onClose();
      }}
    >
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{options.title}</DialogTitle>
          {options.description && (
            <DialogDescription>{options.description}</DialogDescription>
          )}
        </DialogHeader>
        {state.status === 'failed' && (
          <p role="alert" className="text-sm text-destructive">
            {getErrorMessage(
              state.error,
              options.failureMessage ?? t('confirm.failed'),
            )}
          </p>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            {t('common.cancel')}
          </Button>
          <Button
            variant={options.destructive ? 'destructive' : 'default'}
            onClick={handleConfirm}
            disabled={submitting}
          >
            {options.confirmLabel ?? t('confirm.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
