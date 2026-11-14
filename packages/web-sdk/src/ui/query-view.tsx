import {
  AlertTriangle,
  Ban,
  FileQuestion,
  LogIn,
  RotateCcw,
} from 'lucide-react';
import type { ReactNode } from 'react';

import { errorKind } from '@/api/errors';
import { useErrorMessage } from '@/api/use-error-message';
import { useSignIn } from '@/auth/sign-in';
import { type QueryLike, queryState } from '@/hooks/query-state';
import { useTranslation } from '@/i18n';
import { Button } from '@/ui/button';
import { Skeleton } from '@/ui/skeleton';
import { cn } from '@/utils';

export interface QueryErrorStateProps {
  error: unknown;
  /** Fetch again; offered for failures a retry can fix. */
  retry?: () => void;
  /** A retry is under way. */
  retrying?: boolean;
  /** What to say for a 404, e.g. "This submission does not exist". */
  notFound?: string;
  /** What failed, e.g. "Could not load the submissions". */
  failure?: string;
  className?: string;
}

/**
 * A failed fetch, said for what it is: a 401 asks the user to sign in and
 * comes back here afterwards, a 403 says they may not see it, a 404 says it
 * does not exist, and anything else (no connection, a busy or broken
 * server) says so and offers a retry.
 */
export function QueryErrorState({
  error,
  retry,
  retrying = false,
  notFound,
  failure,
  className,
}: QueryErrorStateProps) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const kind = errorKind(error);

  const box = (icon: ReactNode, text: string, action?: ReactNode) => (
    <div
      role={kind === 'notFound' || kind === 'forbidden' ? 'status' : 'alert'}
      className={cn(
        'flex flex-col items-center gap-3 py-10 text-center',
        className,
      )}
    >
      {icon}
      <p className="max-w-md text-sm text-muted-foreground">{text}</p>
      {action}
    </div>
  );
  const iconClass = 'h-8 w-8 text-muted-foreground/60';

  switch (kind) {
    case 'unauthenticated':
      return <SignInPrompt className={className} />;
    case 'forbidden':
      return box(
        <Ban className={iconClass} aria-hidden />,
        t('apiError.kind.forbidden'),
      );
    case 'notFound':
      return box(
        <FileQuestion className={iconClass} aria-hidden />,
        notFound ?? t('apiError.kind.notFound'),
      );
    default:
      return box(
        <AlertTriangle className={iconClass} aria-hidden />,
        errorMessage(error, failure ?? t('common.loadFailed')),
        retry && (
          <Button
            variant="outline"
            size="sm"
            onClick={retry}
            disabled={retrying}
          >
            <RotateCcw className="mr-2 h-4 w-4" aria-hidden />
            {t('common.retry')}
          </Button>
        ),
      );
  }
}

/**
 * What a guest sees in place of something only signed-in users may see: a
 * prompt to sign in that comes back to this page afterwards.
 */
export function SignInPrompt({
  message,
  className,
}: {
  message?: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const signIn = useSignIn();
  return (
    <div
      role="status"
      className={cn(
        'flex flex-col items-center gap-3 py-10 text-center',
        className,
      )}
    >
      <LogIn className="h-8 w-8 text-muted-foreground/60" aria-hidden />
      <p className="max-w-md text-sm text-muted-foreground">
        {message ?? t('query.signInRequired')}
      </p>
      <Button size="sm" onClick={signIn}>
        {t('nav.signIn')}
      </Button>
    </div>
  );
}

/** The default first-load placeholder. */
export function QueryLoading({ className }: { className?: string }) {
  const { t } = useTranslation();
  return (
    <div role="status" aria-busy className={cn('space-y-3', className)}>
      <span className="sr-only">{t('common.loading')}</span>
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-8 w-full" />
    </div>
  );
}

/** An empty list or view: an icon and what is (not) there. */
export function EmptyState({
  icon,
  children,
  className,
}: {
  icon?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground',
        className,
      )}
    >
      {icon}
      {children}
    </div>
  );
}

type QueryViewProps<T> = {
  query: QueryLike<T>;
  /** Shown during the first fetch. Defaults to `QueryLoading`. */
  loading?: ReactNode;
  /** Shown while the query is disabled. Defaults to nothing. */
  idle?: ReactNode;
  notFound?: string;
  failure?: string;
  children: (data: T) => ReactNode;
} & (
  | { isEmpty?: undefined; empty?: undefined }
  | {
      /** Whether the data has nothing to show; `empty` is shown then. */
      isEmpty: (data: T) => boolean;
      empty: ReactNode;
    }
);

/**
 * Render a query in exactly one of its states (see `QueryState`): loading,
 * failed (by kind, with a retry), empty, or its data. Data that a later
 * refetch failed to renew stays on screen under a notice.
 */
export function QueryView<T>({
  query,
  loading,
  idle = null,
  notFound,
  failure,
  isEmpty,
  empty,
  children,
}: QueryViewProps<T>) {
  const { t } = useTranslation();
  const state = queryState(query, isEmpty);
  switch (state.status) {
    case 'idle':
      return idle;
    case 'loading':
      return loading ?? <QueryLoading />;
    case 'error':
      return (
        <QueryErrorState
          error={state.error}
          retry={state.retry}
          retrying={state.retrying}
          notFound={notFound}
          failure={failure}
        />
      );
    case 'empty':
      return empty;
    case 'data':
      return (
        <>
          {state.refreshError != null && (
            <div
              role="status"
              className="mb-3 flex items-center justify-between gap-3 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-muted-foreground"
            >
              <span>{t('query.refreshFailed')}</span>
              <Button variant="ghost" size="sm" onClick={state.retry}>
                {t('common.retry')}
              </Button>
            </div>
          )}
          {children(state.data)}
        </>
      );
  }
}
