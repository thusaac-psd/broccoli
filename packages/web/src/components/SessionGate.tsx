import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { CloudOff, RotateCcw } from 'lucide-react';
import { type ReactNode, useState } from 'react';

/**
 * The page, once the session on this browser is known. While the server
 * cannot be asked whether it still holds, nobody is known and nothing can be
 * fetched, so every page shows this instead of waiting: the session keeps
 * asking by itself, the page continues once it is answered, and the button
 * asks now.
 */
export function SessionGate({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const { unreachable, refresh } = useAuth();
  const [retrying, setRetrying] = useState(false);
  if (!unreachable) return children;

  const retry = () => {
    setRetrying(true);
    void refresh().finally(() => setRetrying(false));
  };

  return (
    <div className="flex min-h-[60vh] flex-col items-center justify-center space-y-6 px-4 text-center">
      <div className="rounded-full bg-muted p-6">
        <CloudOff className="h-12 w-12 text-muted-foreground" aria-hidden />
      </div>
      <div role="alert" className="space-y-2">
        <h1 className="text-3xl font-bold tracking-tighter">
          {t('error.connection.title')}
        </h1>
        <p className="max-w-[600px] text-muted-foreground md:text-xl/relaxed">
          {t('error.connection.retrying')}
        </p>
      </div>
      <Button onClick={retry} disabled={retrying}>
        <RotateCcw className="mr-2 h-4 w-4" aria-hidden />
        {t('error.action.retry')}
      </Button>
    </div>
  );
}
