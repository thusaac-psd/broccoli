import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';
import { type ReactNode, useCallback, useState } from 'react';

import { ConfigIssueList } from '@/features/admin/components/ConfigIssueList';
import {
  configBreakIssues,
  type IncompatibleConfig,
} from '@/features/admin/hooks/config-break';

interface Pending {
  issues: IncompatibleConfig[];
  decide: (force: boolean) => void;
}

/**
 * Sends a change to a problem's test data. When the server refuses it because
 * it would break stored configs that refer to the test data
 * (`CONFIG_WOULD_BREAK`), asks whether to make it anyway, listing them, and
 * on yes sends it again with `force`.
 *
 * Returns `send(write)`, which resolves with the answer to the last request
 * sent, or `null` when the user keeps the data as it is, and the dialog
 * element, which the caller renders once.
 */
export function useTestDataWrite(): [
  <R extends { error?: unknown }>(
    write: (force: boolean) => Promise<R>,
  ) => Promise<R | null>,
  ReactNode,
] {
  const { t } = useTranslation();
  const [pending, setPending] = useState<Pending | null>(null);

  const send = useCallback(
    async <R extends { error?: unknown }>(
      write: (force: boolean) => Promise<R>,
    ): Promise<R | null> => {
      const first = await write(false);
      const issues = configBreakIssues(first.error);
      if (!issues) return first;
      const force = await new Promise<boolean>((decide) =>
        setPending({ issues, decide }),
      );
      setPending(null);
      return force ? write(true) : null;
    },
    [],
  );

  const dialog = (
    <Dialog
      open={pending !== null}
      onOpenChange={(open) => {
        if (!open) pending?.decide(false);
      }}
    >
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('admin.configIssues.breakTitle')}</DialogTitle>
          <DialogDescription>
            {t('admin.configIssues.breakDescription')}
          </DialogDescription>
        </DialogHeader>
        <div className="max-h-72 overflow-y-auto text-sm">
          <ConfigIssueList issues={pending?.issues ?? []} of="problem" />
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => pending?.decide(false)}>
            {t('common.cancel')}
          </Button>
          <Button variant="destructive" onClick={() => pending?.decide(true)}>
            {t('admin.configIssues.breakConfirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return [send, dialog];
}
