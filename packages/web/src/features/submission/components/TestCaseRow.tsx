import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  type SubmissionStatus,
  TestCaseOutput,
  type TestCaseResult,
} from '@broccoli/web-sdk/submission';
import { Button } from '@broccoli/web-sdk/ui';
import { cn, formatKibibytes } from '@broccoli/web-sdk/utils';
import {
  AlertCircle,
  CheckCircle2,
  Clock,
  MinusCircle,
  XCircle,
} from 'lucide-react';
import { useState } from 'react';

import type { VerdictKey } from './verdict-key';
import { getVerdictKey } from './verdict-key';

const VERDICT_CONFIG: Record<
  VerdictKey,
  {
    icon: typeof CheckCircle2;
    color: string;
    bgColor: string;
  }
> = {
  accepted: {
    icon: CheckCircle2,
    color: 'text-green-500',
    bgColor: 'bg-green-500/10',
  },
  wrong_answer: {
    icon: XCircle,
    color: 'text-red-500',
    bgColor: 'bg-red-500/10',
  },
  time_limit: {
    icon: Clock,
    color: 'text-yellow-500',
    bgColor: 'bg-yellow-500/10',
  },
  memory_limit: {
    icon: Clock,
    color: 'text-yellow-500',
    bgColor: 'bg-yellow-500/10',
  },
  runtime_error: {
    icon: AlertCircle,
    color: 'text-orange-500',
    bgColor: 'bg-orange-500/10',
  },
  system_error: {
    icon: AlertCircle,
    color: 'text-gray-500',
    bgColor: 'bg-gray-500/10',
  },
  skipped: {
    icon: MinusCircle,
    color: 'text-gray-400',
    bgColor: 'bg-gray-400/10',
  },
  cancelled: {
    icon: MinusCircle,
    color: 'text-gray-400',
    bgColor: 'bg-gray-400/10',
  },
  custom: {
    icon: AlertCircle,
    color: 'text-blue-500',
    bgColor: 'bg-blue-500/10',
  },
  pending: {
    icon: Clock,
    color: 'text-gray-500',
    bgColor: 'bg-gray-500/10',
  },
};

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
  status: SubmissionStatus;
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
  const verdictKey = getVerdictKey(testCase.verdict, status);
  const config = VERDICT_CONFIG[verdictKey];
  const Icon = config.icon;

  return (
    <div className={cn('rounded-lg border', config.bgColor)}>
      <div className="flex items-center justify-between p-3">
        <div className="flex items-center gap-3">
          <Icon className={cn('h-5 w-5', config.color)} />
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
                {
                  checker_output: 'result.checkerOutput',
                  input: 'result.input',
                  expected_output: 'result.expectedOutput',
                  stdout: 'result.stdout',
                  stderr: 'result.stderr',
                }[field],
              )}
            />
          ))}
        </div>
      )}
    </div>
  );
}
