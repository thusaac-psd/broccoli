import { useTranslation } from '@broccoli/web-sdk/i18n';
import type {
  SubmissionStatus,
  TestCaseResult,
} from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import {
  AlertCircle,
  CheckCircle2,
  Clock,
  MinusCircle,
  XCircle,
} from 'lucide-react';

import type { VerdictKey } from './verdict-key';
import { getVerdictKey } from './verdict-key';

// Cap how much of each test-case text field we render. The server/worker keep up
// to 64 KiB per field; painting that raw in a wrapping <pre> (times many cases)
// janks the page, so the detail view shows a head slice with a truncation note.
const MAX_OUTPUT_CHARS = 2000;

function OutputBlock({ label, text }: { label: string; text: string }) {
  const { t } = useTranslation();
  const truncated = text.length > MAX_OUTPUT_CHARS;
  const shown = truncated ? text.slice(0, MAX_OUTPUT_CHARS) : text;
  return (
    <div>
      <div className="text-xs font-medium text-muted-foreground mb-1">
        {label}
      </div>
      <pre className="text-xs bg-muted p-2 rounded overflow-x-auto whitespace-pre-wrap">
        {shown}
        {truncated && (
          <span className="text-muted-foreground italic">
            {'\n'}
            {t('result.outputTruncated', {
              shown: MAX_OUTPUT_CHARS.toLocaleString(),
              total: text.length.toLocaleString(),
            })}
          </span>
        )}
      </pre>
    </div>
  );
}

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

export function formatMemory(kb: number): string {
  const mb = kb / 1024;
  return mb.toFixed(mb >= 10 ? 0 : 1);
}

export function TestCaseRow({
  testCase,
  index,
  status,
}: {
  testCase: TestCaseResult;
  index: number;
  /**
   * Status of the submission/judgement this test case belongs to. Required
   * so a masked (`null`) verdict can be told apart from a genuinely pending
   * one -- see `getVerdictKey`.
   */
  status: SubmissionStatus;
}) {
  const { t } = useTranslation();
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
            {testCase.checker_output && (
              <div className="text-xs text-muted-foreground mt-1">
                {t('result.checkerOutput')}: {testCase.checker_output}
              </div>
            )}
          </div>
        </div>
        <div className="text-right text-sm text-muted-foreground">
          {testCase.time_used != null && (
            <div>
              {t('result.timeValue', { value: String(testCase.time_used) })}
            </div>
          )}
          {testCase.memory_used != null && (
            <div>
              {t('result.memoryValue', {
                value: formatMemory(testCase.memory_used),
              })}
            </div>
          )}
        </div>
      </div>
      {(testCase.input ||
        testCase.expected_output ||
        testCase.stdout ||
        testCase.stderr) && (
        <div className="px-3 pb-3 space-y-2">
          {testCase.input && (
            <OutputBlock label={t('result.input')} text={testCase.input} />
          )}
          {testCase.expected_output && (
            <OutputBlock
              label={t('result.expectedOutput')}
              text={testCase.expected_output}
            />
          )}
          {testCase.stdout && (
            <OutputBlock label={t('result.stdout')} text={testCase.stdout} />
          )}
          {testCase.stderr && (
            <OutputBlock label={t('result.stderr')} text={testCase.stderr} />
          )}
        </div>
      )}
    </div>
  );
}
