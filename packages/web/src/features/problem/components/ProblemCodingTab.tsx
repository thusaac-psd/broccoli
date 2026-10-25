import { Slot } from '@broccoli/web-sdk/slot';
import type {
  Submission,
  SubmissionSummary,
} from '@broccoli/web-sdk/submission';

import { CodeEditor, type EditorFile } from '@/components/CodeEditor';
import { RecentSubmissionOverview } from '@/features/submission/components/RecentSubmissionOverview';
import { TargetWorkerSelector } from '@/features/submission/components/TargetWorkerSelector';
import type { SubmissionEntry } from '@/features/submission/hooks/use-submissions';

interface ProblemCodingTabProps {
  isCodeFullscreen: boolean;
  onToggleFullscreen: () => void;
  onSubmit: (file: EditorFile, language: string) => void;
  onRun: (
    file: EditorFile,
    language: string,
    customTestCases: { input: string; expected_output?: string | null }[],
  ) => void;
  latestRun: SubmissionEntry | null;
  storageKey: string;
  contestType: string;
  onContestTypeChange?: (value: string) => void;
  contestTypes?: string[];
  submissionFormat?: Record<string, string[]> | null;
  latestSubmission: Submission | null;
  submissionHistory: SubmissionSummary[];
  submissions?: SubmissionEntry[];
  isSubmitting: boolean;
  overviewVisibleCount?: number;
  submissionDetailLinkBuilder?: (submissionId: number) => string;
  contestId?: number;
  problemId: number;
  /**
   * When non-null, the user is admin (`system:admin`) and the worker-pin
   * selector is shown. The array contains currently pinned worker IDs.
   */
  targetWorkers?: string[] | null;
  onTargetWorkersChange?: (next: string[]) => void;
}

export function ProblemCodingTab({
  isCodeFullscreen,
  onToggleFullscreen,
  onSubmit,
  onRun,
  latestRun,
  storageKey,
  contestType,
  onContestTypeChange,
  contestTypes,
  submissionFormat,
  latestSubmission,
  submissionHistory,
  submissions,
  isSubmitting,
  overviewVisibleCount,
  submissionDetailLinkBuilder,
  contestId,
  problemId,
  targetWorkers,
  onTargetWorkersChange,
}: ProblemCodingTabProps) {
  return (
    // Explicit row tracks: when stacked below lg, auto rows let a long
    // submission list claim the height and collapse Monaco (height: 100%).
    <div
      className={`grid flex-1 grid-cols-1 gap-6 overflow-hidden p-6 lg:grid-cols-5 ${isCodeFullscreen ? 'grid-rows-1' : 'grid-rows-[minmax(0,1fr)_fit-content(40%)] lg:grid-rows-1'}`}
    >
      <div
        className={`flex flex-col overflow-hidden ${isCodeFullscreen ? 'lg:col-span-5' : 'lg:col-span-3'}`}
      >
        {targetWorkers != null && onTargetWorkersChange && (
          <div className="mb-2">
            <TargetWorkerSelector
              selected={targetWorkers}
              onChange={onTargetWorkersChange}
              disabled={isSubmitting}
            />
          </div>
        )}
        <CodeEditor
          onSubmit={onSubmit}
          onRun={onRun}
          latestRun={latestRun}
          isFullscreen={isCodeFullscreen}
          onToggleFullscreen={onToggleFullscreen}
          storageKey={storageKey}
          contestType={contestType}
          onContestTypeChange={onContestTypeChange}
          contestTypes={contestTypes}
          submissionFormat={submissionFormat}
        />
      </div>

      {!isCodeFullscreen && (
        <div className="flex min-h-0 flex-col gap-2 overflow-y-auto lg:col-span-2">
          <RecentSubmissionOverview
            entries={submissions ?? []}
            history={submissionHistory}
            isSubmitting={isSubmitting}
            visibleCount={overviewVisibleCount}
            linkBuilder={submissionDetailLinkBuilder}
          />
          <Slot
            name="problem-detail.sidebar"
            as="div"
            className="flex flex-col gap-2"
            slotProps={{
              submission: latestSubmission,
              submissions,
              contestId,
              problemId,
            }}
          />
        </div>
      )}
    </div>
  );
}
