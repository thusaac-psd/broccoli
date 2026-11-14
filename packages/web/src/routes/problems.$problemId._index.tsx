import { useParams } from 'react-router';

import ProblemView from '@/features/problem/components/ProblemView';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('problem.title');

export default function ProblemDetailPage() {
  const { problemId } = useParams();
  return <ProblemView problemId={Number(problemId)} />;
}
