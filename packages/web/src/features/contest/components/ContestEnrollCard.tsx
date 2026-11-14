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
import { useState } from 'react';

const ACCENT = 'hsl(var(--primary))';

export function ContestEnrollCard({
  onEnroll,
  isPending,
  onUnregister,
  isUnregistering,
  showUnregister,
}: {
  onEnroll: () => void;
  isPending: boolean;
  onUnregister?: () => void;
  isUnregistering?: boolean;
  showUnregister?: boolean;
}) {
  const { t } = useTranslation();
  const [confirmOpen, setConfirmOpen] = useState(false);

  const handleUnregisterClick = () => {
    setConfirmOpen(true);
  };

  const handleConfirmUnregister = () => {
    onUnregister?.();
    setConfirmOpen(false);
  };

  return (
    <>
      <div className="rounded-lg border p-4 space-y-3">
        <div>
          <div className="inline-flex items-center gap-1.5 px-2">
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full`}
              style={{ backgroundColor: ACCENT }}
            />
            <span
              className="text-[10px] font-semibold uppercase tracking-[0.15em]"
              style={{ color: ACCENT }}
            >
              {t('contests.enrollTitle')}
            </span>
          </div>
          <p className="text-sm text-muted-foreground mt-1">
            {showUnregister
              ? t('contests.registeredDescription')
              : t('contests.enrollDescription')}
          </p>
        </div>
        {!showUnregister && (
          <Button className="w-full" onClick={onEnroll} disabled={isPending}>
            {isPending ? t('contests.enrolling') : t('contests.enrollAction')}
          </Button>
        )}
        {showUnregister && (
          <Button
            variant="outline"
            className="w-full"
            onClick={handleUnregisterClick}
            disabled={isUnregistering}
          >
            {isUnregistering
              ? t('contests.unregistering')
              : t('contests.unregisterAction')}
          </Button>
        )}
      </div>

      <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('contests.unregisterConfirmTitle')}</DialogTitle>
            <DialogDescription>
              {t('contests.unregisterConfirmDescription')}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setConfirmOpen(false)}
              disabled={isUnregistering}
            >
              {t('common.cancel')}
            </Button>
            <Button
              variant="destructive"
              onClick={handleConfirmUnregister}
              disabled={isUnregistering}
            >
              {t('contests.unregisterConfirm')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
