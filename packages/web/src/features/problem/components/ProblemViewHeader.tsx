import { useAuth } from '@broccoli/web-sdk/auth';
import { useIdentifierLabel, useTranslation } from '@broccoli/web-sdk/i18n';
import { useProblemPresentation } from '@broccoli/web-sdk/problem';
import { Slot } from '@broccoli/web-sdk/slot';
import { formatKibibytes } from '@broccoli/web-sdk/utils';
import type { ReactNode } from 'react';

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
  /**
   * Why the problem is withheld: shown in place of its title and limits,
   * which the header then leaves out instead of showing placeholders.
   */
  withheld?: ReactNode;
}

export function ProblemViewHeader({
  problem,
  headerId,
  contestId,
  withheld,
}: ProblemViewHeaderProps) {
  const { t } = useTranslation();
  const label = useIdentifierLabel();
  const { user } = useAuth();
  const { hideLabels } = useProblemPresentation(contestId, user?.id);

  const timeLimit = problem ? `${problem.time_limit} ms` : '—';
  const memoryLimit = problem ? formatKibibytes(problem.memory_limit) : '—';

  return (
    <div className="shrink-0 px-4 pt-4 pb-4 md:px-6 md:pt-6">
      {/* On a phone the countdown wraps below the title rather than being
          hidden: a contestant needs the time left on every screen. */}
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1 basis-64">
          {withheld ? (
            <div className="space-y-2">
              <h1 className="text-2xl font-bold tracking-tight">
                {hideLabels ? t('problem.title') : headerId}
              </h1>
              {withheld}
            </div>
          ) : (
            <ProblemHeader
              id={hideLabels ? undefined : headerId}
              title={problem?.title ?? t('problem.title')}
              type={problem ? label('problemType', problem.problem_type) : '—'}
              io={t('problem.standardIo')}
              timeLimit={timeLimit}
              memoryLimit={memoryLimit}
            />
          )}
        </div>
        {contestId && <ContestCountdownMini />}
      </div>
      <Slot name="problem-detail.header" as="div" className="relative mt-3" />
    </div>
  );
}
