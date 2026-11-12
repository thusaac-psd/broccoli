/**
 * Replaces the `tokens` object field with a cohesive token configuration panel.
 * Shows different fields based on the selected token mode, with a visual
 * token budget indicator.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedBadge } from '@broccoli/web-sdk/slot';
import { Input, Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';

import { getConfiguredScoringMode } from './config-rules';

interface TokenValue {
  mode?: string;
  initial?: number;
  max?: number;
  regen_interval_min?: number;
}

const TOKEN_MODES = [
  {
    key: 'none',
    labelKey: 'ioi.tokenConfig.mode.none.label',
    descKey: 'ioi.tokenConfig.mode.none.desc',
  },
  {
    key: 'fixed_budget',
    labelKey: 'ioi.tokenConfig.mode.fixedBudget.label',
    descKey: 'ioi.tokenConfig.mode.fixedBudget.desc',
  },
  {
    key: 'regenerating',
    labelKey: 'ioi.tokenConfig.mode.regenerating.label',
    descKey: 'ioi.tokenConfig.mode.regenerating.desc',
  },
] as const;

export function TokenConfigPanel({
  value,
  schema,
  onChange,
  formValues,
  showAsPlaceholder,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const val: TokenValue = (
    typeof value === 'object' && value !== null ? value : {}
  ) as TokenValue;
  const scoringMode = getConfiguredScoringMode(formValues);
  const tokensRequired = scoringMode === 'best_tokened_or_last';
  const mode = val.mode ?? 'none';
  const initial = val.initial ?? 2;
  const max = val.max ?? 5;

  const update = (patch: Partial<TokenValue>) => {
    onChange({ ...val, ...patch });
  };

  const isActive = mode !== 'none';

  return (
    <div
      className="flex flex-col gap-3.5 col-span-2 p-4 rounded-[10px] transition-all duration-300 ease-[cubic-bezier(0.4,0,0.2,1)]"
      style={{
        border: `1px solid ${isActive ? 'color-mix(in srgb, var(--primary, #4f46e5) 30%, var(--border, #e5e7eb))' : 'var(--border, #e5e7eb)'}`,
        background: isActive
          ? 'color-mix(in srgb, var(--primary, #4f46e5) 2%, var(--card, #fff))'
          : 'var(--card, #fff)',
      }}
    >
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <div className="text-[11px] font-semibold uppercase tracking-wide opacity-55">
            {schema.title ?? t('ioi.tokenConfig.title')}
          </div>
          {showAsPlaceholder && inheritedSource && (
            <InheritedBadge source={inheritedSource} />
          )}
        </div>
        {isActive && (
          <div className="flex items-center gap-1.5 flex-wrap">
            <StatPill
              label={t('ioi.tokenConfig.initial')}
              value={String(initial)}
            />
            {mode === 'regenerating' && (
              <StatPill
                label={t('ioi.tokenConfig.maximum')}
                value={String(max)}
              />
            )}
          </div>
        )}
      </div>

      {/* Mode selector */}
      <div className="grid grid-cols-3 rounded-lg border border-border overflow-hidden bg-muted">
        {TOKEN_MODES.map((m, i) => {
          const isCurrent = mode === m.key;
          const isDisabled = tokensRequired && m.key === 'none';
          return (
            <button
              key={m.key}
              type="button"
              onClick={() => {
                if (!isDisabled) {
                  update({ mode: m.key });
                }
              }}
              disabled={isDisabled}
              className={cn(
                'py-2 px-1 border-none text-xs text-center flex flex-col items-center gap-0.5 relative transition-all duration-150',
                isCurrent ? 'bg-card font-semibold' : 'bg-transparent',
                isDisabled
                  ? 'cursor-not-allowed opacity-35'
                  : isCurrent
                    ? 'opacity-100 cursor-pointer'
                    : 'opacity-60 cursor-pointer',
              )}
              style={{
                borderRight:
                  i < TOKEN_MODES.length - 1
                    ? '1px solid var(--border, #e5e7eb)'
                    : 'none',
                color: 'inherit',
              }}
            >
              <span>{t(m.labelKey)}</span>
              <span className="text-[9px] opacity-50">{t(m.descKey)}</span>
              {isCurrent && (
                <div className="absolute bottom-0 left-[20%] right-[20%] h-0.5 rounded-t-sm bg-primary" />
              )}
            </button>
          );
        })}
      </div>

      {/* None mode */}
      {mode === 'none' && (
        <p className="text-xs opacity-45 m-0 italic text-center py-2">
          {t('ioi.tokenConfig.disabledMessage')}
        </p>
      )}

      {tokensRequired && (
        <p className="text-xs m-0 text-muted-foreground leading-normal">
          {t('ioi.tokenConfig.requiredForScoringMode')}
        </p>
      )}

      {/* Fixed budget mode */}
      {mode === 'fixed_budget' && (
        <div className="flex flex-col gap-2.5">
          <div>
            <Label className="block mb-1 text-[10px] font-semibold uppercase tracking-wide opacity-50">
              {t('ioi.tokenConfig.initialTokens')}
            </Label>
            <Input
              type="number"
              min={0}
              max={100}
              value={initial}
              onChange={(e) =>
                update({ initial: parseInt(e.target.value) || 0 })
              }
              className="tabular-nums"
            />
            <div className="text-[10px] opacity-40 mt-0.5">
              {t('ioi.tokenConfig.fixedDuration')}
            </div>
          </div>
        </div>
      )}

      {isActive && (
        <div className="text-[11px] opacity-50 italic text-center py-1">
          {t('ioi.tokenConfig.feedbackNote')}
        </div>
      )}

      {/* Regenerating mode */}
      {mode === 'regenerating' && (
        <div className="flex flex-col gap-3">
          {/* Regen visualization: dots filling up */}
          <div className="flex items-center gap-1.5 py-2 px-3 rounded-md bg-muted justify-center">
            <span className="text-[10px] opacity-40 ml-1 italic">
              {t('ioi.tokenConfig.regenRate', {
                minutes: val.regen_interval_min ?? 30,
              })}
            </span>
          </div>

          <div className="grid grid-cols-3 gap-2.5">
            <div>
              <Label className="block mb-1 text-[10px] font-semibold uppercase tracking-wide opacity-50">
                {t('ioi.tokenConfig.initial')}
              </Label>
              <Input
                type="number"
                min={0}
                max={100}
                value={initial}
                onChange={(e) =>
                  update({ initial: parseInt(e.target.value) || 0 })
                }
                className="tabular-nums"
              />
              <div className="text-[10px] opacity-40 mt-0.5">
                {t('ioi.tokenConfig.startingTokens')}
              </div>
            </div>
            <div>
              <Label className="block mb-1 text-[10px] font-semibold uppercase tracking-wide opacity-50">
                {t('ioi.tokenConfig.maximum')}
              </Label>
              <Input
                type="number"
                min={0}
                max={100}
                value={max}
                onChange={(e) => update({ max: parseInt(e.target.value) || 0 })}
                className="tabular-nums"
              />
              <div className="text-[10px] opacity-40 mt-0.5">
                {t('ioi.tokenConfig.cap')}
              </div>
            </div>
            <div>
              <Label className="block mb-1 text-[10px] font-semibold uppercase tracking-wide opacity-50">
                {t('ioi.tokenConfig.interval')}
              </Label>
              <Input
                type="number"
                min={1}
                max={1440}
                value={val.regen_interval_min ?? 30}
                onChange={(e) =>
                  update({ regen_interval_min: parseInt(e.target.value) || 1 })
                }
                className="tabular-nums"
              />
              <div className="text-[10px] opacity-40 mt-0.5">
                {t('ioi.tokenConfig.minutesPerToken')}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function StatPill({
  label,
  value,
}: Readonly<{ label: string; value: string }>) {
  return (
    <div
      className="inline-flex items-center gap-1.5 text-[10px] font-medium px-2 py-0.5 rounded-[10px]"
      style={{
        background:
          'color-mix(in srgb, var(--primary, #4f46e5) 10%, transparent)',
        color: 'var(--primary, #4f46e5)',
      }}
    >
      <span className="opacity-70">{label}</span>
      <span>{value}</span>
    </div>
  );
}
