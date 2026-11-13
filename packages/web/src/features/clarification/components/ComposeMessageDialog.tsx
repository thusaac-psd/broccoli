import { useAsyncDialog } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@broccoli/web-sdk/ui';
import { type ReactNode, useState } from 'react';

import { MessageField } from '@/features/clarification/components/MessageField';
import { isSendableMessage } from '@/features/clarification/message';

/**
 * A dialog that composes and sends one message. The text is cleared and the
 * dialog closed only once `onSubmit` resolves; when it rejects (it reports
 * its own failure), the dialog stays open with the text, ready to retry.
 */
export function ComposeMessageDialog({
  trigger,
  title,
  placeholder,
  hint,
  submitLabel,
  onSubmit,
  ready = true,
  onSent,
  children,
}: {
  trigger: ReactNode;
  title: string;
  placeholder: string;
  hint: string;
  submitLabel: string;
  onSubmit: (content: string) => Promise<unknown>;
  /** Whether the fields in `children` are complete. */
  ready?: boolean;
  /** Runs after a successful send, to clear the fields in `children`. */
  onSent?: () => void;
  /** Extra fields shown above the message. */
  children?: ReactNode;
}) {
  const { t } = useTranslation();
  const dialog = useAsyncDialog();
  const [content, setContent] = useState('');
  const canSend = ready && isSendableMessage(content) && !dialog.submitting;

  const handleSubmit = async () => {
    if (!canSend) return;
    if (await dialog.submit(() => onSubmit(content))) {
      setContent('');
      onSent?.();
    }
  };

  return (
    <Dialog open={dialog.open} onOpenChange={dialog.onOpenChange}>
      <DialogTrigger asChild>{trigger}</DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-4">
          {children}
          <MessageField
            value={content}
            onChange={setContent}
            placeholder={placeholder}
            hint={hint}
            disabled={dialog.submitting}
          />
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            disabled={dialog.submitting}
            onClick={() => dialog.onOpenChange(false)}
          >
            {t('common.cancel')}
          </Button>
          <Button onClick={handleSubmit} disabled={!canSend}>
            {dialog.submitting ? t('clarification.sending') : submitLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
