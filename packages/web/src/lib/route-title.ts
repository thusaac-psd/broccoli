import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useParams } from 'react-router';

import { useContestInfo } from '@/features/contest/hooks/use-contest-info';

/**
 * What every route module exports as `handle`. The deepest matched route
 * that has one names the page in the browser tab.
 */
export interface RouteHandle {
  /**
   * The page's name, or undefined while the data it comes from loads; a
   * route without one (the home page) shows just the app's name. It is a
   * hook, called on every render of the title, so it may read the route's
   * params and the query cache.
   */
  useTitle?: () => string | undefined;
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
