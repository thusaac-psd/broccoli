import { Button } from '@broccoli/web-sdk/ui';
import { RotateCcw, ServerCrash, Undo2 } from 'lucide-react';
import { Component, type ErrorInfo, type ReactNode } from 'react';

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
 * app context: strings are plain English and navigation uses raw window APIs,
 * so the fallback can never crash itself. It visually mirrors AppErrorPage.
 */
const defaultErrorFallback = (
  <div className="flex flex-col items-center justify-center min-h-[60vh] px-4 text-center space-y-6">
    <div className="p-6 rounded-full bg-muted">
      <ServerCrash className="w-12 h-12 text-muted-foreground" />
    </div>

    <div className="space-y-2">
      <h1 className="text-3xl font-bold tracking-tighter sm:text-4xl">
        Something went wrong
      </h1>
      <p className="text-gray-500 md:text-xl/relaxed dark:text-gray-400 max-w-[600px]">
        The page hit an unexpected error. Your submissions and contest data are
        safe on the server; reloading usually fixes it.
      </p>
    </div>

    <div className="mt-8 flex justify-center gap-4">
      <Button onClick={() => window.location.reload()} variant="default">
        <RotateCcw className="mr-2 h-4 w-4" />
        Reload page
      </Button>
      <Button onClick={() => window.history.back()} variant="outline">
        <Undo2 className="mr-2 h-4 w-4" />
        Go back
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
      return defaultErrorFallback;
    }
    return this.props.children;
  }
}
