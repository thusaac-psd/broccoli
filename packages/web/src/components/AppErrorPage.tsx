import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { CloudOff, Loader2, RotateCcw, ServerCrash, Undo2 } from 'lucide-react';

interface AppErrorPageProps {
  /** The error was a failure to load code (offline, or a redeploy). */
  chunkLoad: boolean;
  /** The page is already reloading to fetch fresh code. */
  reloading: boolean;
}

/** What the root error boundary shows, in the user's language. */
export function AppErrorPage({ chunkLoad, reloading }: AppErrorPageProps) {
  const { t } = useTranslation();
  const Icon = reloading ? Loader2 : chunkLoad ? CloudOff : ServerCrash;

  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] px-4 text-center space-y-6">
      <div className="p-6 rounded-full bg-muted">
        <Icon
          className={`w-12 h-12 text-muted-foreground ${reloading ? 'animate-spin' : ''}`}
        />
      </div>

      <div className="space-y-2">
        <h1 className="text-3xl font-bold tracking-tighter sm:text-4xl">
          {chunkLoad ? t('error.chunk.title') : t('error.crash.title')}
        </h1>
        <p className="text-gray-500 md:text-xl/relaxed dark:text-gray-400 max-w-[600px]">
          {reloading
            ? t('error.chunk.reloading')
            : chunkLoad
              ? t('error.chunk.desc')
              : t('error.crash.desc')}
        </p>
      </div>

      {!reloading && (
        <div className="mt-8 flex justify-center gap-4">
          <Button onClick={() => window.location.reload()} variant="default">
            <RotateCcw className="mr-2 h-4 w-4" />
            {t('error.action.reload')}
          </Button>
          <Button onClick={() => window.history.back()} variant="outline">
            <Undo2 className="mr-2 h-4 w-4" />
            {t('error.action.back')}
          </Button>
        </div>
      )}
    </div>
  );
}
