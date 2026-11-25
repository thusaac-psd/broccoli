import type { ApiSchemas } from '@broccoli/web-sdk/api';
import {
  pluginKeys,
  pluginName,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import { AlertTriangle } from 'lucide-react';

import type { JsonSchema } from '@/components/config/types';
import { useFieldName } from '@/components/config/use-field-name';
import { useFieldProblemText } from '@/components/config/use-field-problem-text';
import { serverProblem } from '@/components/config/utils';

type IncompatibleConfig = ApiSchemas['IncompatibleConfig'];

/**
 * Whose config issues a list shows: a problem's, or a contest's, which names
 * each of its problems by its label there.
 */
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
  const { t, lookup } = useTranslation();
  const pluginText = usePluginText();
  const problemText = useFieldProblemText();
  const fieldName = useFieldName();
  /** The config as its settings form names it: its plugin and its tab. */
  function configName(issue: IncompatibleConfig, schema?: JsonSchema) {
    const title = pluginText(
      pluginKeys.configTitle(issue.plugin_id, issue.namespace),
      schema?.title ?? issue.namespace,
    );
    return `${pluginName(lookup, issue.plugin_id)} › ${title}`;
  }
  function where(issue: IncompatibleConfig, config: string) {
    if (issue.scope === 'contest') {
      return t('admin.configIssues.onContest', { config });
    }
    if (issue.scope === 'contest_problem') {
      return of === 'contest'
        ? t('admin.configIssues.forProblem', {
            config,
            problem: issue.label ?? String(issue.problem_id ?? ''),
          })
        : t('admin.configIssues.inContest', {
            config,
            contestId: String(issue.contest_id ?? ''),
          });
    }
    return t('admin.configIssues.onProblem', { config });
  }
  return (
    <ul className="space-y-2">
      {issues.map((issue) => {
        const schema = (issue.json_schema ?? undefined) as
          | JsonSchema
          | undefined;
        const config = configName(issue, schema);
        // A field the schema no longer names reads as its raw path.
        const field = (path: string) =>
          schema
            ? fieldName(
                {
                  pluginId: issue.plugin_id,
                  namespace: issue.namespace,
                  schema,
                },
                path.replace(/\[(\d+)\]/g, '.$1'),
              )
            : path;
        return (
          <li
            key={`${issue.scope}/${issue.ref_id}/${issue.plugin_id}/${issue.namespace}`}
          >
            <span className="font-medium">{where(issue, config)}</span>
            <ul className="list-disc pl-5">
              {issue.errors.map((error) => (
                <li key={`${error.path}:${error.message}`}>
                  {error.path ? (
                    <span className="font-medium">{field(error.path)}</span>
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
