import { LOCALE_STORAGE_KEY } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { RotateCcw, ServerCrash, Undo2 } from 'lucide-react';
import { Component, type ErrorInfo, type ReactNode } from 'react';

import type { MessageKey } from '@/lib/i18n/catalog';
import { CORE_CATALOGS } from '@/lib/i18n/catalogs';
import { en } from '@/lib/i18n/en';
import { reportError } from '@/lib/telemetry';

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
}

/**
 * Last-resort error screen.
 *
 * The root error boundary wraps its translated page (and the providers that
 * page needs) in this boundary, and the crash being reported may have
 * originated inside one of those providers. It therefore deliberately uses no
 * app context: its strings come from the saved locale's translations when
 * they are already cached (else English), and navigation uses raw window
 * APIs, so the fallback can never crash itself. It visually mirrors
 * AppErrorPage.
 */
/** Translate `key` without the i18n context, from the bundled catalogs. */
function t(key: MessageKey): string {
  try {
    const locale = localStorage.getItem(LOCALE_STORAGE_KEY);
    return (locale ? CORE_CATALOGS[locale]?.[key] : undefined) ?? en[key];
  } catch {
    return en[key];
  }
}

const ErrorFallback = () => (
  <div className="flex flex-col items-center justify-center min-h-[60vh] px-4 text-center space-y-6">
    <div className="p-6 rounded-full bg-muted">
      <ServerCrash className="w-12 h-12 text-muted-foreground" />
    </div>

    <div className="space-y-2">
      <h1 className="text-3xl font-bold tracking-tighter sm:text-4xl">
        {t('errorReporter.title')}
      </h1>
      <p className="text-gray-500 md:text-xl/relaxed dark:text-gray-400 max-w-[600px]">
        {t('errorReporter.description')}
      </p>
    </div>

    <div className="mt-8 flex justify-center gap-4">
      <Button onClick={() => window.location.reload()} variant="default">
        <RotateCcw className="mr-2 h-4 w-4" />
        {t('errorReporter.reload')}
      </Button>
      <Button onClick={() => window.history.back()} variant="outline">
        <Undo2 className="mr-2 h-4 w-4" />
        {t('errorReporter.back')}
      </Button>
    </div>
  </div>
);

export class ErrorReporter extends Component<Props, State> {
  state: State = { hasError: false };

  static getDerivedStateFromError(): State {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The anti-FOUC script in root.tsx sets the document's opacity to 0 for
    // non-English users until the locale loads. Never let a crash leave the
    // error screen invisible.
    if (typeof document !== 'undefined') {
      document.documentElement.style.opacity = '';
    }

    reportError({
      message: error.message,
      stack: info.componentStack ?? error.stack,
    });
  }

  render() {
    if (this.state.hasError) {
      return <ErrorFallback />;
    }
    return this.props.children;
  }
}
