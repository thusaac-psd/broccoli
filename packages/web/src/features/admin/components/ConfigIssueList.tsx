import type { ApiSchemas } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { AlertTriangle } from 'lucide-react';

import { useFieldProblemText } from '@/components/config/use-field-problem-text';
import { serverProblem } from '@/components/config/utils';

type IncompatibleConfig = ApiSchemas['IncompatibleConfig'];

/** Whose config issues a list shows: a problem's, or a contest's. */
export type ConfigIssuesOf = 'problem' | 'contest';

/**
 * Configs their plugins find something wrong with, each with where it is set
 * and what is wrong with it, in the UI language.
 */
export function ConfigIssueList({
  issues,
  of,
}: {
  issues: readonly IncompatibleConfig[];
  of: ConfigIssuesOf;
}) {
  const { t } = useTranslation();
  const problemText = useFieldProblemText();
  function where(issue: IncompatibleConfig, config: string) {
    if (issue.scope === 'contest') {
      return t('admin.configIssues.onContest', { config });
    }
    if (issue.scope === 'contest_problem') {
      const [contestId, problemId] = issue.ref_id.split(':');
      return of === 'contest'
        ? t('admin.configIssues.forProblem', { config, problemId })
        : t('admin.configIssues.inContest', { config, contestId });
    }
    return t('admin.configIssues.onProblem', { config });
  }
  return (
    <ul className="space-y-2">
      {issues.map((issue) => {
        const config = `${issue.plugin_id}/${issue.namespace}`;
        return (
          <li key={`${issue.scope}/${issue.ref_id}/${config}`}>
            <span className="font-medium">{where(issue, config)}</span>
            <ul className="list-disc pl-5">
              {issue.errors.map((error) => (
                <li key={`${error.path}:${error.message}`}>
                  {error.path ? (
                    <code className="font-mono text-xs">{error.path}</code>
                  ) : null}
                  {error.path ? ': ' : null}
                  {problemText(serverProblem(error))}
                </li>
              ))}
            </ul>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * A warning listing a problem's or contest's config issues, under `title`.
 * Nothing when there are none.
 */
export function ConfigIssuesAlert({
  title,
  issues,
  of,
}: {
  title: string;
  issues: readonly IncompatibleConfig[] | undefined;
  of: ConfigIssuesOf;
}) {
  if (!issues || issues.length === 0) return null;
  return (
    <div
      role="alert"
      className="mb-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-300"
    >
      <p className="flex items-center gap-2 font-medium">
        <AlertTriangle className="h-4 w-4 shrink-0" />
        {title}
      </p>
      <div className="mt-2">
        <ConfigIssueList issues={issues} of={of} />
      </div>
    </div>
  );
}
