import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { Code2 } from 'lucide-react';
import { Link } from 'react-router';

import { useRegistrationOpen } from '@/features/auth/hooks/use-registration-status';

export function GuestWelcome() {
  const { t } = useTranslation();
  const registrationOpen = useRegistrationOpen();

  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] text-center px-4">
      <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary/10 mb-6">
        <Code2 className="h-8 w-8 text-primary" />
      </div>
      <h1 className="text-3xl font-bold tracking-tight mb-2">
        {t('homepage.welcome')}
      </h1>
      <p className="text-muted-foreground max-w-md mb-8">
        {t('homepage.welcomeDesc')}
      </p>
      <div className="flex gap-3">
        <Button size="lg" asChild>
          <Link to="/login">{t('nav.signIn')}</Link>
        </Button>
        {registrationOpen && (
          <Button size="lg" variant="outline" asChild>
            <Link to="/register">{t('nav.signUp')}</Link>
          </Button>
        )}
      </div>
    </div>
  );
}
