import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Card,
  CardContent,
  QueryLoading,
  SignInPrompt,
} from '@broccoli/web-sdk/ui';
import { Shield } from 'lucide-react';

/**
 * What a page shows a viewer who may not see it. While the session is being
 * restored nobody is known yet, so it waits; a guest is asked to sign in (and
 * comes back here); only a signed-in user is told they lack permission.
 */
export function Unauthorized() {
  const { t } = useTranslation();
  const { user, isLoading } = useAuth();
  if (isLoading) return <QueryLoading className="p-6" />;
  if (!user) return <SignInPrompt />;
  return (
    <div className="flex items-center justify-center h-full">
      <Card className="max-w-md">
        <CardContent className="pt-6 text-center">
          <Shield className="mx-auto h-12 w-12 text-muted-foreground mb-4" />
          <p className="text-destructive text-lg font-medium">
            {t('admin.unauthorized')}
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
