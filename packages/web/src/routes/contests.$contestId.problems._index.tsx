import { useParams } from 'react-router';

import { AdminProblemListView } from '@/features/admin/components/AdminProblemListView';
import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle('contests.problems');

export default function ContestProblemListPage() {
  const { contestId } = useParams();
  return <AdminProblemListView contestId={Number(contestId)} />;
}
