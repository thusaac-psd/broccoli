import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { MessageCircle } from 'lucide-react';

import { ComposeMessageDialog } from '@/features/clarification/components/ComposeMessageDialog';

export function AskQuestionDialog({
  onSubmit,
}: {
  onSubmit: (content: string) => Promise<unknown>;
}) {
  const { t } = useTranslation();
  return (
    <ComposeMessageDialog
      trigger={
        <Button>
          <MessageCircle className="h-4 w-4 mr-2" />
          {t('clarification.ask.trigger')}
        </Button>
      }
      title={t('clarification.ask.title')}
      placeholder={t('clarification.ask.placeholder')}
      hint={t('clarification.ask.hint')}
      submitLabel={t('clarification.send')}
      onSubmit={onSubmit}
    />
  );
}
