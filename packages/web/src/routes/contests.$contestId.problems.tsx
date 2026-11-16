import { Outlet } from 'react-router';

import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle('contests.problems');

export default function ContestProblemsLayout() {
  return <Outlet />;
}
