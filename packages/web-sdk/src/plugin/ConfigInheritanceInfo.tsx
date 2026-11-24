/**
 * Generic config cascade display for admin config forms.
 *
 * Plugins register this component (or a thin wrapper) in the `config.form.*`
 * slot. It shows which parent scope provides the effective inherited value and
 * displays the full cascade hierarchy.
 */
import { useTranslation } from '@/i18n';
import { configSourceName } from '@/plugin/config-source';
import {
  type ConfigFormSlotProps,
  inheritedValueOf,
} from '@/slot/config-field-slot-props';
import { cn } from '@/utils';

/**
 * The `config.form.*` slot's props, which a wrapper passes through as they
 * come, and the one field the display is about.
 */
export interface ConfigInheritanceInfoProps
  extends Partial<
    Pick<
      ConfigFormSlotProps,
      'scope' | 'schema' | 'hasExplicitValue' | 'inherited'
    >
  > {
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

/** The value a scope's row stores for `key`, if it stores one. */
function storedField(stored: unknown, key: string): unknown | undefined {
  if (typeof stored !== 'object' || stored === null) return undefined;
  const v = (stored as Record<string, unknown>)[key];
  return v !== undefined && v !== null ? v : undefined;
}

export function ConfigInheritanceInfo({
  scope,
  hasExplicitValue,
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

  const inheritedValue = inheritedValueOf(fieldKey, inherited);
  const effective = inheritedValue
    ? {
        value: inheritedValue.value,
        source:
          inheritedValue.source === 'contest'
            ? labels.contest
            : inheritedValue.source === 'problem'
              ? labels.problem
              : configSourceName(inheritedValue.source, t),
      }
    : null;

  // A field this scope stores overrides the parent scopes; one it does not
  // store inherits from them.
  const hasLocalOverride = hasExplicitValue?.(fieldKey) ?? false;
  const activeInherited = hasLocalOverride ? null : effective;

  const properties = schema?.properties as
    | Record<string, { default?: unknown }>
    | undefined;
  const schemaDefault = properties?.[fieldKey]?.default;
  const defaultLabel =
    schemaDefault !== undefined ? formatValue(schemaDefault) : null;

  const contest = inherited?.levels.contest;
  const problem = inherited?.levels.problem;
  const contestValue = storedField(contest?.stored, fieldKey);
  const problemValue = storedField(problem?.stored, fieldKey);
  const contestDisabled = contest?.enabled === false;
  const problemDisabled = problem?.enabled === false;

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
                <span className="text-[11px] text-muted-foreground italic">
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
