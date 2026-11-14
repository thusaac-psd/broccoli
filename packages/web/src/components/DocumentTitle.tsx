import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMatches } from 'react-router';

import type { RouteHandle } from '@/lib/route-title';

const noTitle = () => undefined;

function RouteTitle({ useTitle }: { useTitle: () => string | undefined }) {
  const { t } = useTranslation();
  const page = useTitle();
  const app = t('app.name');
  return <title>{page ? t('app.pageTitle', { page, app }) : app}</title>;
}

/** Keeps the document title on the name of the page shown. */
export function DocumentTitle() {
  const match = [...useMatches()]
    .reverse()
    .find((m) => typeof m.handle === 'object' && m.handle !== null);
  // Keyed by route, so a different page's `useTitle` is a new component and
  // the hooks it calls never change between renders.
  return (
    <RouteTitle
      key={match?.id}
      useTitle={(match?.handle as RouteHandle | undefined)?.useTitle ?? noTitle}
    />
  );
}
