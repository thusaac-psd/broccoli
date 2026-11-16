import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { AlertTriangle, Loader2, RotateCw, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { toast } from 'sonner';

import { useDlqMessage } from '@/features/dlq/hooks/useDlqMessage';
import { messageTypeMeta } from '@/features/dlq/utils/messageType';

interface Props {
  messageId: number | null;
  onOpenChange: (open: boolean) => void;
}

export function DlqMessageDetailDialog({ messageId, onOpenChange }: Props) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const open = messageId !== null;

  const { data, isLoading, error } = useDlqMessage(messageId);
  const meta = data ? messageTypeMeta(data.message_type) : null;

  const [busy, setBusy] = useState(false);
  const [confirm, confirmDialog] = useConfirmDialog();

  async function handleRetry() {
    if (!messageId) return;
    setBusy(true);
    try {
      const { error } = await apiClient.POST('/dlq/{id}/retry', {
        params: { path: { id: messageId } },
      });
      if (error) {
        toast.error(getErrorMessage(error, t('dlq.detail.retryError')));
        return;
      }
      toast.success(t('dlq.detail.retrySuccess'));
      await queryClient.invalidateQueries({ queryKey: ['dlq'] });
      onOpenChange(false);
    } finally {
      setBusy(false);
    }
  }

  function handleDelete() {
    if (!messageId) return;
    confirm({
      title: t('dlq.detail.deleteConfirm'),
      confirmLabel: t('dlq.detail.deleteAction'),
      destructive: true,
      failureMessage: t('dlq.detail.deleteError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE('/dlq/{id}', {
          params: { path: { id: messageId } },
        });
        if (error) throw error;
        toast.success(t('dlq.detail.deleteSuccess'));
        await queryClient.invalidateQueries({ queryKey: ['dlq'] });
        onOpenChange(false);
      },
    });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t('dlq.detail.title')}</DialogTitle>
          <DialogDescription>{t('dlq.detail.description')}</DialogDescription>
        </DialogHeader>

        {isLoading && (
          <div className="flex items-center justify-center py-10 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
          </div>
        )}

        {error && (
          <div className="flex items-start gap-3 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm">
            <AlertTriangle className="h-4 w-4 shrink-0 text-destructive" />
            <p>{t('dlq.detail.loadError')}</p>
          </div>
        )}

        {data && (
          <div className="space-y-4 text-sm">
            <div className="grid grid-cols-2 gap-3">
              <Field label={t('dlq.detail.messageId')}>
                <span className="font-mono text-xs">{data.message_id}</span>
              </Field>
              <Field label={t('dlq.detail.type')}>
                {meta && (
                  <span className="inline-flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-0.5 text-xs">
                    <meta.icon className="h-3 w-3 text-muted-foreground" />
                    {t(meta.labelKey)}
                  </span>
                )}
              </Field>
              <Field label={t('dlq.detail.errorCode')}>
                <span className="font-medium">{data.error_code}</span>
              </Field>
              <Field label={t('dlq.detail.retries')}>
                <span className="tabular-nums">{data.retry_count}</span>
              </Field>
              <Field label={t('dlq.detail.firstFailedAt')}>
                <span className="font-mono text-xs">
                  {new Date(data.first_failed_at).toLocaleString()}
                </span>
              </Field>
              <Field label={t('dlq.detail.lastFailedAt')}>
                <span className="font-mono text-xs">
                  {new Date(data.last_failed_at).toLocaleString()}
                </span>
              </Field>
              <Field label={t('dlq.detail.createdAt')}>
                <span className="font-mono text-xs">
                  {new Date(data.created_at).toLocaleString()}
                </span>
              </Field>
              {data.submission_id !== null && (
                <Field label={t('dlq.detail.submissionId')}>
                  <Link
                    to={`/submissions/${data.submission_id}`}
                    className="font-mono text-xs text-primary hover:underline"
                  >
                    #{data.submission_id}
                  </Link>
                </Field>
              )}
              <Field label={t('dlq.detail.status')}>
                {data.resolved ? (
                  <Badge variant="secondary">
                    {data.resolved_by === null
                      ? t('dlq.detail.resolvedAutomatically')
                      : t('dlq.detail.resolvedByUser', {
                          id: data.resolved_by,
                        })}
                  </Badge>
                ) : (
                  <Badge
                    variant="outline"
                    className="border-destructive/50 text-destructive"
                  >
                    {t('dlq.row.unresolved')}
                  </Badge>
                )}
              </Field>
            </div>

            <Field label={t('dlq.detail.errorMessage')}>
              <pre className="whitespace-pre-wrap rounded-md border bg-muted/40 p-2 text-xs">
                {data.error_message}
              </pre>
            </Field>

            <Field label={t('dlq.detail.payload')}>
              <pre className="max-h-64 overflow-auto rounded-md border bg-muted/40 p-2 text-[11px] font-mono">
                {JSON.stringify(data.payload, null, 2)}
              </pre>
            </Field>

            <Field label={t('dlq.detail.retryHistory')}>
              <pre className="max-h-48 overflow-auto rounded-md border bg-muted/40 p-2 text-[11px] font-mono">
                {JSON.stringify(data.retry_history, null, 2)}
              </pre>
            </Field>
          </div>
        )}

        <DialogFooter className="gap-2 sm:gap-2">
          <Button
            variant="outline"
            onClick={handleDelete}
            disabled={busy || !data || data.resolved}
          >
            <Trash2 className="h-4 w-4 mr-2" />
            {t('dlq.detail.deleteAction')}
          </Button>
          {meta?.retryable && (
            <Button
              onClick={handleRetry}
              disabled={busy || !data || data.resolved}
            >
              {busy ? (
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
              ) : (
                <RotateCw className="h-4 w-4 mr-2" />
              )}
              {t('dlq.detail.retryAction')}
            </Button>
          )}
        </DialogFooter>
        {confirmDialog}
      </DialogContent>
    </Dialog>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0">
      <p className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </p>
      <div className="mt-1 min-w-0">{children}</div>
    </div>
  );
}
