import { Outlet } from 'react-router';

import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('problem.title');

export default function ProblemDetailPage() {
  return <Outlet />;
}
