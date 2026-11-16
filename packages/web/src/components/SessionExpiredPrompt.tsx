import { useAuth, useSignIn } from '@broccoli/web-sdk/auth';
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

/**
 * Tells the user when the server ended their session (rather than letting
 * them silently become a guest), and takes them to sign in and back to this
 * page. Unsaved code is kept: the editor's drafts live in this browser.
 */
export function SessionExpiredPrompt() {
  const { t } = useTranslation();
  const { sessionExpired } = useAuth();
  const signIn = useSignIn();
  const [dismissed, setDismissed] = useState(false);

  // Each new expiry asks again.
  const [lastExpired, setLastExpired] = useState(sessionExpired);
  if (lastExpired !== sessionExpired) {
    setLastExpired(sessionExpired);
    if (sessionExpired) setDismissed(false);
  }

  return (
    <Dialog
      open={sessionExpired && !dismissed}
      onOpenChange={(open) => {
        if (!open) setDismissed(true);
      }}
    >
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('session.expired.title')}</DialogTitle>
          <DialogDescription>
            {t('session.expired.description')}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => setDismissed(true)}>
            {t('session.expired.later')}
          </Button>
          <Button onClick={signIn}>{t('nav.signIn')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
