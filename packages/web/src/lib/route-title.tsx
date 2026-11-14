import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMatches, useParams } from 'react-router';

import { useContestInfo } from '@/features/contest/hooks/use-contest-info';

/**
 * What every route module exports as `handle`. The deepest matched route
 * that has one names the page in the browser tab.
 */
export interface RouteHandle {
  /**
   * The page's name, or undefined while the data it comes from loads. It is
   * a hook, called on every render of the title, so it may read the route's
   * params and the query cache.
   */
  useTitle: () => string | undefined;
}

function isRouteHandle(handle: unknown): handle is RouteHandle {
  return (
    typeof handle === 'object' &&
    handle !== null &&
    typeof (handle as RouteHandle).useTitle === 'function'
  );
}

/** A page named by one translation key. */
export function pageTitle(key: string): RouteHandle {
  return {
    useTitle: function usePageTitle() {
      return useTranslation().t(key);
    },
  };
}

/**
 * A page of the contest in the `:contestId` param: the page's name (when
 * given) and then the contest's. The contest comes from the query every
 * contest page shares, so naming the tab costs no request.
 */
export function contestPageTitle(key?: string): RouteHandle {
  return {
    useTitle: function useContestPageTitle() {
      const { t } = useTranslation();
      const contestTitle = useContestTitle();
      if (!key) return contestTitle;
      return contestTitle
        ? t('app.pageInContest', { page: t(key), contest: contestTitle })
        : t(key);
    },
  };
}

/** The title of the contest in the `:contestId` param, once loaded. */
export function useContestTitle(): string | undefined {
  const { contestId } = useParams();
  return useContestInfo(Number(contestId)).contest?.title;
}

function RouteTitle({ useTitle }: RouteHandle) {
  const { t } = useTranslation();
  const page = useTitle();
  const app = t('app.name');
  return <title>{page ? t('app.pageTitle', { page, app }) : app}</title>;
}

/** Keeps the document title on the name of the page shown. */
export function DocumentTitle() {
  const match = [...useMatches()]
    .reverse()
    .find((m) => isRouteHandle(m.handle));
  if (!match) return <RouteTitle useTitle={() => undefined} />;
  // Keyed by route, so a different page's `useTitle` is a new component and
  // the hooks it calls never change between renders.
  const { useTitle } = match.handle as RouteHandle;
  return <RouteTitle key={match.id} useTitle={useTitle} />;
}
