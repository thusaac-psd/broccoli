import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { QueryLoading } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { Trophy } from 'lucide-react';
import { useEffect } from 'react';
import { Navigate } from 'react-router';

import { ListSkeleton } from '@/components/ListSkeleton';
import { GuestWelcome } from '@/features/auth/components/GuestWelcome';
import { ContestSelector } from '@/features/contest/components/ContestSelector';
import { useContest } from '@/features/contest/contexts/contest-context';
import { type RouteHandle } from '@/lib/route-title';

/** The home page is the app itself: the tab shows just its name. */
export const handle: RouteHandle = {};

export default function Index() {
  const { t } = useTranslation();
  const { user, isLoading: sessionLoading } = useAuth();
  const apiClient = useApiClient();
  const { contestId, setContest } = useContest();

  const { data: contests, isLoading: isContestsLoading } = useQuery({
    queryKey: ['dashboard-contests'],
    enabled: !!user,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests', {
        params: {
          query: {
            page: 1,
            per_page: 100,
            sort_by: 'start_time',
            sort_order: 'desc',
          },
        },
      });
      if (error) throw error;
      return data.data;
    },
  });

  // FIX: check permissions instead of roles
  const isAdmin = !!user?.roles.includes('admin');
  const onlyContest = !isAdmin && contests?.length === 1 ? contests[0] : null;

  // Remember the only contest, so the sidebar keeps showing it.
  useEffect(() => {
    if (onlyContest && contestId !== onlyContest.id) {
      setContest(onlyContest.id, onlyContest.title);
    }
  }, [onlyContest, contestId, setContest]);

  // Redirects render as `<Navigate>`, never as an effect that calls
  // `navigate`: this page stays mounted while the navigation is pending, and
  // an effect that re-ran on each of those renders would navigate again and
  // again.
  if (isAdmin) {
    return <Navigate to="/admin" replace />;
  }

  // Nobody is known until the session is restored.
  if (sessionLoading) {
    return <QueryLoading className="p-6" />;
  }

  // Not logged in
  if (!user) {
    return <GuestWelcome />;
  }

  // Loading contests
  if (isContestsLoading) {
    return (
      <div className="flex flex-col gap-6 p-6">
        <h1 className="text-2xl font-bold">{t('homepage.title')}</h1>
        <ListSkeleton />
      </div>
    );
  }

  // With a single contest there is nothing to select on this page, so
  // always go to its dashboard (even when it is already selected, e.g. after
  // clicking the logo link to `/`).
  if (onlyContest) {
    return <Navigate to={`/contests/${onlyContest.id}`} replace />;
  }

  // No contests
  if (!contests?.length) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] text-center px-4">
        <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-muted mb-6">
          <Trophy className="h-8 w-8 text-muted-foreground" />
        </div>
        <h1 className="text-2xl font-bold tracking-tight mb-2">
          {t('homepage.title')}
        </h1>
        <p className="text-muted-foreground max-w-md">
          {t('homepage.noContests')}
        </p>
      </div>
    );
  }

  // Multiple contests: always show the selector, even when a contest is
  // already selected, so this page can be used to switch contests (the
  // sidebar links here for exactly that purpose).
  return (
    <div className="flex flex-col gap-6 p-6 max-w-2xl mx-auto">
      <ContestSelector contests={contests} />
    </div>
  );
}
