import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import {
  isKnownOffline,
  subscribeToOnlineStatus,
} from '@broccoli/web-sdk/utils';
import {
  CloudOff,
  Loader2,
  RotateCcw,
  ServerCrash,
  Undo2,
  WifiOff,
} from 'lucide-react';
import { useSyncExternalStore } from 'react';

import { type ChunkRecovery, retryPageLoad } from '@/lib/chunk-reload';

interface AppErrorPageProps {
  /** The error was a failure to load code (offline, or a redeploy). */
  chunkLoad: boolean;
  /** For a failure to load code, where recovering from it stands. */
  phase: ChunkRecovery;
}

/** What the root error boundary shows, in the user's language. */
export function AppErrorPage({ chunkLoad, phase }: AppErrorPageProps) {
  const { t } = useTranslation();
  const offline = useSyncExternalStore(
    subscribeToOnlineStatus,
    isKnownOffline,
    () => false,
  );
  const waiting = chunkLoad && phase === 'waiting';
  const reloading = chunkLoad && phase === 'reloading';
  const Icon = reloading
    ? Loader2
    : chunkLoad
      ? offline
        ? WifiOff
        : CloudOff
      : ServerCrash;

  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] px-4 text-center space-y-6">
      <div className="p-6 rounded-full bg-muted">
        <Icon
          className={`w-12 h-12 text-muted-foreground ${reloading ? 'animate-spin' : ''}`}
        />
      </div>

      <div
        role={waiting || reloading ? 'status' : 'alert'}
        className="space-y-2"
      >
        <h1 className="text-3xl font-bold tracking-tighter sm:text-4xl">
          {waiting
            ? t('error.connection.title')
            : chunkLoad
              ? t('error.chunk.title')
              : t('error.crash.title')}
        </h1>
        <p className="text-gray-500 md:text-xl/relaxed dark:text-gray-400 max-w-[600px]">
          {reloading
            ? t('error.chunk.reloading')
            : waiting
              ? t('error.connection.retrying')
              : chunkLoad
                ? t('error.chunk.desc')
                : t('error.crash.desc')}
        </p>
      </div>

      {!waiting && !reloading && (
        <div className="mt-8 flex justify-center gap-4">
          <Button onClick={() => retryPageLoad()} variant="default">
            <RotateCcw className="mr-2 h-4 w-4" />
            {chunkLoad ? t('error.action.retry') : t('error.action.reload')}
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
