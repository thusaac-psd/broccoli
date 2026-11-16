import { Outlet } from 'react-router';

import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('sidebar.problems');

export default function ProblemsLayout() {
  return <Outlet />;
}
