import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { Megaphone } from 'lucide-react';

import { ComposeMessageDialog } from '@/features/clarification/components/ComposeMessageDialog';

export function PostAnnouncementDialog({
  onSubmit,
}: {
  onSubmit: (content: string) => Promise<unknown>;
}) {
  const { t } = useTranslation();
  return (
    <ComposeMessageDialog
      trigger={
        <Button variant="outline">
          <Megaphone className="h-4 w-4 mr-2" />
          {t('clarification.announce.trigger')}
        </Button>
      }
      title={t('clarification.announce.title')}
      placeholder={t('clarification.announce.placeholder')}
      hint={t('clarification.announce.hint')}
      submitLabel={t('clarification.announce.submit')}
      onSubmit={onSubmit}
    />
  );
}
