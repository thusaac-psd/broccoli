import type { I18nContextValue } from '@/i18n';
import type {
  SubmissionError,
  SubmissionStatus,
  SubmissionStatusFilterValue,
  Withheld,
} from '@/submission/types';

/** Statuses after which a submission, judgement or code run will never change again. */
export const TERMINAL_STATUSES: ReadonlySet<SubmissionStatus> =
  new Set<SubmissionStatus>([
    'Judged',
    'CompilationError',
    'SystemError',
    'Superseded',
  ]);

/**
 * Whether a status will never change again. It takes a status the server
 * showed: whether a withheld one (`null`) can still change depends on why it
 * is withheld, which `submissionPhase` decides.
 */
export function isTerminalStatus(status: SubmissionStatus): boolean {
  return TERMINAL_STATUSES.has(status);
}

/** What a submission whose outcome is withheld shows in place of it. */
export function withheldLabel(
  withheld: Withheld | null | undefined,
  t: I18nContextValue['t'],
): string {
  if (withheld?.reason !== 'plugin_outage') return t('submission.resultHidden');
  return withheld.outage.state === 'disabled'
    ? t('submission.resultUnavailableDisabled')
    : t('submission.resultUnavailable');
}

function parseSubmissionErrorValue(value: unknown): SubmissionError | null {
  if (!value) return null;
  if (typeof value === 'string') {
    try {
      return parseSubmissionErrorValue(JSON.parse(value));
    } catch {
      return null;
    }
  }
  if (typeof value !== 'object') return null;
  const obj = value as Record<string, unknown>;
  if (typeof obj.code === 'string' && typeof obj.message === 'string') {
    const result: SubmissionError = { code: obj.code, message: obj.message };
    if (obj.details != null && typeof obj.details === 'object') {
      result.details = obj.details as Record<string, unknown>;
    }
    return result;
  }
  return (
    parseSubmissionErrorValue(obj.error) ??
    parseSubmissionErrorValue(obj.data) ??
    parseSubmissionErrorValue(obj.body) ??
    parseSubmissionErrorValue(obj.response)
  );
}

/**
 * Best-effort parse of the server's `{ code, message, details? }` error
 * envelope, unwrapping common nesting (`error`/`data`/`body`/`response`)
 * and JSON-encoded strings along the way.
 */
export function parseSubmissionError(err: unknown): SubmissionError {
  const parsed = parseSubmissionErrorValue(err);
  if (parsed) return parsed;
  return { code: 'UNKNOWN', message: String(err) };
}

/**
 * The i18n key of each submission status. Typed against the API schema's
 * `SubmissionStatus`, so a status the server adds or drops fails to compile
 * here instead of falling through to a wrong label.
 */
const STATUS_LABEL_KEYS = {
  Queued: 'result.queued',
  Pending: 'result.pending',
  Compiling: 'result.compilingShort',
  Running: 'result.runningShort',
  Judged: 'result.judged',
  CompilationError: 'result.compilationError',
  SystemError: 'result.systemError',
  Superseded: 'result.superseded',
} as const satisfies Record<SubmissionStatus, string>;

/**
 * Every status a submission shows, in lifecycle order: all but `Superseded`,
 * which only a judgement a newer one replaced has.
 */
const SUBMISSION_STATUSES = (
  Object.keys(STATUS_LABEL_KEYS) as SubmissionStatus[]
).filter((status) => status !== 'Superseded');

export const SUBMISSION_STATUS_FILTER_OPTIONS: readonly SubmissionStatusFilterValue[] =
  ['all', ...SUBMISSION_STATUSES];

export function getStatusLabel(
  status: SubmissionStatusFilterValue,
  t: I18nContextValue['t'],
): string {
  return status === 'all'
    ? t('submissions.filters.allStatuses')
    : t(STATUS_LABEL_KEYS[status]);
}

export function toSubmissionStatus(
  value: SubmissionStatusFilterValue,
): SubmissionStatus | undefined {
  return value === 'all' ? undefined : value;
}
