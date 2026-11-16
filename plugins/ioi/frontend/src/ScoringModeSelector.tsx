/**
 * Replaces the `scoring_mode` dropdown with a visual timeline selector.
 * Three IOI scoring eras, rendered as a horizontal timeline with selectable cards.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedAnnotation, InheritedBadge } from '@broccoli/web-sdk/slot';
import { Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';

import { getConfiguredTokenMode } from './config-rules';

const MODES = [
  {
    key: 'best_tokened_or_last',
    titleKey: 'ioi.scoringMode.bestTokenedOrLast.title',
    eraKey: 'ioi.scoringMode.bestTokenedOrLast.era',
    formula: 'max(tokened, last)',
    descriptionKey: 'ioi.scoringMode.bestTokenedOrLast.description',
    accent: '#0ea5e9',
  },
  {
    key: 'max_submission',
    titleKey: 'ioi.scoringMode.maxSubmission.title',
    eraKey: 'ioi.scoringMode.maxSubmission.era',
    formula: 'max(s\u2081 \u2026 s\u2099)',
    descriptionKey: 'ioi.scoringMode.maxSubmission.description',
    accent: '#8b5cf6',
  },
  {
    key: 'sum_best_subtask',
    titleKey: 'ioi.scoringMode.sumBestSubtask.title',
    eraKey: 'ioi.scoringMode.sumBestSubtask.era',
    formula: '\u03A3 max(st\u1d62)',
    descriptionKey: 'ioi.scoringMode.sumBestSubtask.description',
    accent: '#10b981',
  },
] as const;

/** Accent for modes that are neither selected nor inherited. */
const INACTIVE_ACCENT = 'var(--muted-foreground, #9ca3af)';

export function ScoringModeSelector({
  value,
  schema,
  onChange,
  formValues,
  setFieldValue,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const selected = typeof value === 'string' ? value : '';
  const inherited =
    showAsPlaceholder && typeof inheritedValue === 'string'
      ? inheritedValue
      : null;
  const tokenMode = getConfiguredTokenMode(formValues);

  const handleSelect = (mode: string) => {
    onChange(mode);

    if (
      mode === 'best_tokened_or_last' &&
      tokenMode === 'none' &&
      setFieldValue
    ) {
      setFieldValue(['tokens', 'mode'], 'fixed_budget');
    }
  };

  return (
    <div className="flex flex-col gap-2.5 col-span-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-semibold uppercase tracking-wider opacity-55">
          {schema.title ?? t('ioi.scoringMode.label')}
        </Label>
        {showAsPlaceholder && inheritedSource && (
          <InheritedBadge source={inheritedSource} />
        )}
      </div>
      {schema.description && (
        <p className="text-xs opacity-50 m-0 leading-normal">
          {schema.description}
        </p>
      )}

      {/* Timeline connector */}
      <div className="relative">
        {/* Horizontal line behind cards */}
        <div className="absolute top-[18px] left-4 right-4 h-0.5 bg-border z-0" />

        <div className="grid grid-cols-3 gap-3 relative z-[1]">
          {MODES.map((mode) => {
            const isSelected = !showAsPlaceholder && selected === mode.key;
            const isInherited = inherited === mode.key;
            const accentColor =
              isSelected || isInherited ? mode.accent : INACTIVE_ACCENT;

            return (
              <button
                key={mode.key}
                type="button"
                onClick={() => handleSelect(mode.key)}
                className="flex flex-col items-stretch gap-0 border-none p-0 text-left cursor-pointer bg-transparent"
                style={{ color: 'inherit' }}
              >
                {/* Timeline dot */}
                <div className="flex justify-center mb-2.5">
                  <div
                    style={{
                      width: isSelected ? '14px' : '10px',
                      height: isSelected ? '14px' : '10px',
                      borderRadius: '50%',
                      background: isSelected
                        ? mode.accent
                        : 'var(--background, #fff)',
                      border: `2px solid ${accentColor}`,
                      transition: 'all 0.2s cubic-bezier(0.4, 0, 0.2, 1)',
                      boxShadow: isSelected
                        ? `0 0 0 3px color-mix(in srgb, ${mode.accent} 20%, transparent)`
                        : 'none',
                    }}
                  />
                </div>

                {/* Era label */}
                <div
                  className="text-[10px] font-semibold tracking-[0.04em] text-center mb-2 tabular-nums"
                  style={{ color: accentColor }}
                >
                  {t(mode.eraKey)}
                </div>

                {/* Card body */}
                <div
                  className={cn(
                    'rounded-lg p-3 flex-1 transition-all duration-200 ease-[cubic-bezier(0.4,0,0.2,1)]',
                    isInherited && !isSelected && 'opacity-50',
                  )}
                  style={{
                    border: isSelected
                      ? `1.5px solid ${mode.accent}`
                      : isInherited
                        ? `1.5px dashed ${mode.accent}40`
                        : '1px solid var(--border, #e5e7eb)',
                    background: isSelected
                      ? `color-mix(in srgb, ${mode.accent} 4%, var(--card, #fff))`
                      : isInherited
                        ? `color-mix(in srgb, ${mode.accent} 2%, var(--card, #fff))`
                        : 'var(--card, #fff)',
                  }}
                >
                  {/* Formula badge */}
                  <div
                    className="text-[11px] font-mono font-medium py-0.5 px-2 rounded inline-block mb-2 tracking-[0.01em] transition-all duration-200"
                    style={{
                      background: isSelected
                        ? `color-mix(in srgb, ${mode.accent} 12%, transparent)`
                        : 'var(--muted, #f3f4f6)',
                      color: isSelected
                        ? mode.accent
                        : 'var(--muted-foreground, #6b7280)',
                    }}
                  >
                    {mode.formula}
                  </div>

                  <div
                    className={cn(
                      'text-[13px] font-semibold mb-1',
                      isSelected ? 'opacity-100' : 'opacity-80',
                    )}
                  >
                    {t(mode.titleKey)}
                  </div>
                  <div
                    className={cn(
                      'text-[11px] leading-normal transition-opacity duration-200',
                      isSelected ? 'opacity-65' : 'opacity-45',
                    )}
                  >
                    {t(mode.descriptionKey)}
                  </div>
                </div>
              </button>
            );
          })}
        </div>

        <details className="text-[11px] opacity-60 mt-2">
          <summary className="cursor-pointer">
            {t('ioi.scoringMode.howItWorks')}
          </summary>
          <p className="mt-1.5 leading-relaxed m-0">
            {t('ioi.scoringMode.explanation')}
          </p>
        </details>
        {inheritedSource && (
          <InheritedAnnotation
            source={inheritedSource}
            value={String(inheritedValue ?? '')}
            isOverride={!showAsPlaceholder}
          />
        )}
      </div>
    </div>
  );
}
