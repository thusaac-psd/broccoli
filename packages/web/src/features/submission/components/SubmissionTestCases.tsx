import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  type SubmissionStatus,
  type TestCaseResult,
  useTestCasePage,
} from '@broccoli/web-sdk/submission';
import { PaginatedList, QueryErrorState } from '@broccoli/web-sdk/ui';
import { type ReactNode, useState } from 'react';

import { TestCaseRow } from './TestCaseRow';

export function SubmissionTestCases({
  submissionId,
  judgementId,
  live = false,
  status,
  comparisonJudgementId,
  renderDiff,
}: {
  submissionId: number;
  judgementId?: number | null;
  live?: boolean;
  status: SubmissionStatus;
  comparisonJudgementId?: number;
  renderDiff?: (
    testCase: TestCaseResult,
    current?: TestCaseResult,
  ) => ReactNode;
}) {
  const { t } = useTranslation();
  const [page, setPage] = useState(1);
  const query = useTestCasePage({ submissionId, judgementId, page, live });
  const cases = query.data?.test_case_results ?? [];
  const comparison = useTestCasePage({
    submissionId,
    judgementId: comparisonJudgementId,
    testCaseIds: cases.flatMap((tc) =>
      tc.test_case_id == null ? [] : [tc.test_case_id],
    ),
    enabled:
      comparisonJudgementId != null && comparisonJudgementId !== judgementId,
  });
  const current = new Map(
    comparison.data?.test_case_results.map((tc) => [tc.test_case_id, tc]),
  );
  if (query.isLoading)
    return (
      <div className="py-3 text-sm text-muted-foreground">
        {t('result.loadingOutput')}
      </div>
    );
  if (query.isError && !query.data)
    return (
      <QueryErrorState
        className="py-3"
        error={query.error}
        retry={() => void query.refetch()}
        retrying={query.isFetching}
        failure={t('result.caseLoadError')}
      />
    );
  return (
    <PaginatedList
      items={cases}
      pagination={query.data?.pagination}
      onPageChange={setPage}
      loading={query.isFetching}
    >
      {(rows, offset) =>
        rows.map((tc, index) => (
          <div key={tc.id} className="space-y-1">
            {comparison.isSuccess &&
              renderDiff?.(tc, current.get(tc.test_case_id))}
            <TestCaseRow
              testCase={tc}
              index={offset + index + 1}
              submissionId={submissionId}
              judgementId={judgementId}
              status={status}
            />
          </div>
        ))
      }
    </PaginatedList>
  );
}
