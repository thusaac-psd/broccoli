import type { ApiSchemas } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';

import { useFieldProblemText } from '@/components/config/use-field-problem-text';
import { serverProblem } from '@/components/config/utils';

type IncompatibleConfig = ApiSchemas['IncompatibleConfig'];

/**
 * Stored configs their plugins refuse, each with where it is set and what is
 * wrong with it, in the UI language.
 */
export function ConfigIssueList({
  issues,
}: {
  issues: readonly IncompatibleConfig[];
}) {
  const { t } = useTranslation();
  const problemText = useFieldProblemText();
  return (
    <ul className="space-y-2">
      {issues.map((issue) => {
        const config = `${issue.plugin_id}/${issue.namespace}`;
        const contestId =
          issue.scope === 'contest_problem'
            ? issue.ref_id.split(':')[0]
            : undefined;
        return (
          <li key={`${issue.scope}/${issue.ref_id}/${config}`}>
            <span className="font-medium">
              {contestId
                ? t('admin.configIssues.inContest', { config, contestId })
                : t('admin.configIssues.onProblem', { config })}
            </span>
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
