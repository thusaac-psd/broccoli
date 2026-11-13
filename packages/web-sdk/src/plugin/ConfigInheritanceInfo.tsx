/**
 * Generic config cascade display for admin config forms.
 *
 * Plugins register this component (or a thin wrapper) in the `config.form.*`
 * slot. It shows which parent scope provides the effective inherited value and
 * displays the full cascade hierarchy.
 */
import { useTranslation } from '@/i18n';
import type { InheritedConfig } from '@/slot/config-field-slot-props';
import { resolveInheritedValue } from '@/slot/config-field-slot-props';
import { cn } from '@/utils';

type ConfigScope =
  | { scope: 'plugin'; pluginId: string }
  | { scope: 'contest'; contestId: number }
  | { scope: 'problem'; problemId: number }
  | { scope: 'contest_problem'; contestId: number; problemId: number };

export interface ConfigInheritanceInfoProps {
  scope?: ConfigScope;
  pluginId?: string;
  namespace?: string;
  schema?: { properties?: Record<string, { default?: unknown }> };
  hasExplicitValue?: (path: string | string[]) => boolean;
  /**
   * Whether the current scope stores no explicit values at all. The cascade
   * is wholesale: storing ANY field at this scope makes the whole local
   * config effective, so no field inherits from parent scopes.
   */
  isUsingDefaultsOnly?: boolean;
  inherited?: InheritedConfig;
  fieldKey: string;
  formatValue: (value: unknown) => string;
  labels: {
    contest: string;
    problem: string;
    contestHint: string;
    inheritInfo: (value: string, source: string) => string;
    overrideInfo: string;
    notSet: string;
    notSetWithDefault: (defaultLabel: string) => string;
    active: string;
  };
}

function extractField(
  config: Record<string, unknown> | null | undefined,
  key: string,
): unknown | undefined {
  if (!config) return undefined;
  const v = config[key];
  return v !== undefined && v !== null ? v : undefined;
}

export function ConfigInheritanceInfo({
  scope,
  hasExplicitValue,
  isUsingDefaultsOnly,
  inherited,
  schema,
  fieldKey,
  formatValue,
  labels,
}: ConfigInheritanceInfoProps) {
  const { t } = useTranslation();
  const isContestProblem = scope?.scope === 'contest_problem';
  const isContest = scope?.scope === 'contest';

  if (!isContestProblem && !isContest) return null;

  if (isContest) {
    return (
      <p className="m-0 text-xs text-muted-foreground">{labels.contestHint}</p>
    );
  }

  const rawEffective = resolveInheritedValue(fieldKey, inherited);
  const effective = rawEffective
    ? {
        value: rawEffective.value,
        source:
          rawEffective.source === 'Contest' ? labels.contest : labels.problem,
      }
    : null;

  const hasLocalOverride = hasExplicitValue?.(fieldKey) ?? false;
  // The judging cascade is wholesale: if ANY field is stored at this scope,
  // the whole local config is effective and no field inherits from a parent
  // scope (unset local fields resolve to the schema default instead).
  const localConfigWins =
    isUsingDefaultsOnly === undefined ? hasLocalOverride : !isUsingDefaultsOnly;
  const activeInherited = localConfigWins ? null : effective;

  const schemaDefault = schema?.properties?.[fieldKey]?.default;
  const defaultLabel =
    schemaDefault !== undefined ? formatValue(schemaDefault) : null;

  const contestValue = extractField(inherited?.contest?.values, fieldKey);
  const problemValue = extractField(inherited?.problem?.values, fieldKey);
  const contestDisabled = inherited?.contest?.enabled === false;
  const problemDisabled = inherited?.problem?.enabled === false;

  const rows: {
    label: string;
    value: unknown;
    disabled: boolean;
  }[] = inherited
    ? [
        {
          label: labels.contest,
          value: contestValue,
          disabled: contestDisabled,
        },
        {
          label: labels.problem,
          value: problemValue,
          disabled: problemDisabled,
        },
      ]
    : [];

  return (
    <div
      className={cn(
        'text-xs text-muted-foreground flex flex-col',
        rows.length > 0 ? 'gap-1.5' : 'gap-0',
      )}
    >
      <p className="m-0">
        {hasLocalOverride
          ? labels.overrideInfo
          : activeInherited
            ? labels.inheritInfo(
                formatValue(activeInherited.value),
                activeInherited.source,
              )
            : defaultLabel
              ? labels.notSetWithDefault(defaultLabel)
              : labels.notSet}
      </p>
      {rows.length > 0 && (
        <div className="flex flex-col gap-0.5">
          {rows.map((row) => (
            <div key={row.label} className="flex gap-1.5 items-baseline">
              <span
                className={cn(
                  'font-medium min-w-13',
                  row.disabled && 'line-through opacity-60',
                )}
              >
                {row.label}:
              </span>
              {row.value !== undefined ? (
                <code
                  className={cn(
                    'bg-muted px-1.5 rounded text-[11px]',
                    row.disabled && 'line-through opacity-60',
                  )}
                >
                  {formatValue(row.value)}
                </code>
              ) : (
                <span className="italic text-[11px]">
                  {defaultLabel
                    ? labels.notSetWithDefault(defaultLabel)
                    : labels.notSet}
                </span>
              )}
              {row.disabled && (
                <span className="text-[11px] text-muted-foreground/60 italic">
                  {t('config.inheritance.disabled')}
                </span>
              )}
              {!row.disabled && row.label === activeInherited?.source && (
                <span className="text-[11px] text-primary">
                  ← {labels.active}
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
