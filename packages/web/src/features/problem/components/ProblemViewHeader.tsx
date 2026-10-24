import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useProblemPresentation } from '@broccoli/web-sdk/problem';
import { Slot } from '@broccoli/web-sdk/slot';
import { formatKibibytes } from '@broccoli/web-sdk/utils';

import { ContestCountdownMini } from '@/features/contest/components/ContestCountdown';

import { ProblemHeader } from './ProblemHeader';

interface ProblemViewHeaderProps {
  problem:
    | {
        id: number;
        title: string;
        problem_type: string;
        time_limit: number;
        memory_limit: number;
      }
    | undefined;
  headerId: string;
  contestId?: number;
}

export function ProblemViewHeader({
  problem,
  headerId,
  contestId,
}: ProblemViewHeaderProps) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const { hideLabels } = useProblemPresentation(contestId, user?.id);

  const timeLimit = problem ? `${problem.time_limit} ms` : '—';
  const memoryLimit = problem ? formatKibibytes(problem.memory_limit) : '—';

  return (
    <div className="shrink-0 px-6 pt-6 pb-4">
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <ProblemHeader
            id={hideLabels ? undefined : headerId}
            title={problem?.title ?? t('problem.title')}
            type={problem?.problem_type ?? '—'}
            io="Standard Input / Output"
            timeLimit={timeLimit}
            memoryLimit={memoryLimit}
          />
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {contestId && (
            <div className="hidden lg:flex items-center">
              <ContestCountdownMini />
            </div>
          )}
        </div>
      </div>
      <Slot name="problem-detail.header" as="div" className="relative mt-3" />
    </div>
  );
}
