import { AdminProblemListView } from '@/features/admin/components/AdminProblemListView';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('sidebar.problems');

export default function ProblemListPage() {
  return <AdminProblemListView />;
}
