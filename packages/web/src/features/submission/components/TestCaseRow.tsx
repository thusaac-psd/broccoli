import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  type ShownStatus,
  TestCaseOutput,
  type TestCaseResult,
  verdictStyle,
} from '@broccoli/web-sdk/submission';
import { Button } from '@broccoli/web-sdk/ui';
import { cn, formatKibibytes } from '@broccoli/web-sdk/utils';
import { useState } from 'react';

/** The outputs a test case's details show, in order. */
const DETAIL_FIELDS = [
  'checker_output',
  'input',
  'expected_output',
  'stdout',
  'stderr',
] as const;

export function TestCaseRow({
  testCase,
  index,
  submissionId,
  judgementId,
  status,
}: {
  testCase: TestCaseResult;
  index: number;
  submissionId: number;
  judgementId?: number | null;
  status: ShownStatus;
}) {
  const { t } = useTranslation();
  const [detailsOpen, setDetailsOpen] = useState(false);
  const hasDetails = !!(
    testCase.checker_output ||
    testCase.input ||
    testCase.expected_output ||
    testCase.stdout ||
    testCase.stderr
  );
  const style = verdictStyle(testCase.verdict, status);
  const Icon = style.icon;

  return (
    <div className={cn('rounded-lg border', style.surface)}>
      <div className="flex items-center justify-between p-3">
        <div className="flex items-center gap-3">
          <Icon className={cn('h-5 w-5', style.text)} />
          <div>
            <div className="font-medium">
              {t('result.testCase', { id: String(index) })}
            </div>
          </div>
        </div>
        <div className="text-right text-sm text-muted-foreground">
          {testCase.time_used != null && (
            <div>
              {t('result.timeValue', { value: String(testCase.time_used) })}
            </div>
          )}
          {testCase.memory_used != null && (
            <div>{formatKibibytes(testCase.memory_used)}</div>
          )}
        </div>
      </div>
      {hasDetails && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mx-3 mb-2"
          aria-expanded={detailsOpen}
          onClick={() => setDetailsOpen((open) => !open)}
        >
          {t(detailsOpen ? 'result.hideDetails' : 'result.showDetails')}
        </Button>
      )}
      {hasDetails && detailsOpen && (
        <div className="px-3 pb-3 space-y-2">
          {DETAIL_FIELDS.map((field) => (
            <TestCaseOutput
              key={field}
              submissionId={submissionId}
              judgementId={judgementId}
              testCase={testCase}
              field={field}
              label={t(
                (
                  {
                    checker_output: 'result.checkerOutput',
                    input: 'result.input',
                    expected_output: 'result.expectedOutput',
                    stdout: 'result.stdout',
                    stderr: 'result.stderr',
                  } as const
                )[field],
              )}
            />
          ))}
        </div>
      )}
    </div>
  );
}
