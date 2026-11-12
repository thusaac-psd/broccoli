/**
 * Replaces the `mode` dropdown with a visual card selector.
 * Each mode gets a card with an icon, name, and description.
 *
 * Receives slot props: { value, schema, onChange, showAsPlaceholder, inheritedValue, inheritedSource, ... }
 */

import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedAnnotation, InheritedBadge } from '@broccoli/web-sdk/slot';
import { Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';

/** Icon glyph and description translation key for each known mode. */
const MODE_INFO: Record<string, { icon: string; descriptionKey: string }> = {
  fast: {
    icon: '\u26A1',
    descriptionKey: 'configUiTest.modeCard.fast.description',
  },
  balanced: {
    icon: '\u2696\uFE0F',
    descriptionKey: 'configUiTest.modeCard.balanced.description',
  },
  thorough: {
    icon: '\uD83D\uDD0D',
    descriptionKey: 'configUiTest.modeCard.thorough.description',
  },
  experimental: {
    icon: '\uD83E\uDDEA',
    descriptionKey: 'configUiTest.modeCard.experimental.description',
  },
};

/** Glyph shown for a mode the plugin has no card info for. */
const UNKNOWN_MODE_ICON = '\u2753';

export function ModeCardSelector({
  value,
  schema,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  // `schema.enum` is `unknown[] | undefined` in the shared slot type (JSON
  // schema enum values are arbitrary in general); this field is always a
  // list of mode-name strings in practice, so narrow it defensively rather
  // than trusting the schema's declared type or casting past it.
  const modes = (schema.enum ?? Object.keys(MODE_INFO)).filter(
    (mode): mode is string => typeof mode === 'string',
  );
  const selected = typeof value === 'string' ? value : '';
  const inherited =
    showAsPlaceholder && typeof inheritedValue === 'string'
      ? inheritedValue
      : null;

  return (
    <div className="flex flex-col gap-1.5 col-span-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-medium uppercase tracking-wide opacity-60">
          {schema.title ?? t('configUiTest.modeCard.defaultTitle')}
          <span className="ml-1.5 text-[10px] font-normal normal-case tracking-normal text-amber-600">
            {t('configUiTest.pluginOverrideBadge')}
          </span>
        </Label>
        {showAsPlaceholder && inheritedSource && (
          <InheritedBadge source={inheritedSource} />
        )}
      </div>
      {schema.description && (
        <p className="text-xs opacity-60 m-0">{schema.description}</p>
      )}
      <div className="grid grid-cols-2 gap-2">
        {modes.map((mode) => {
          const info = MODE_INFO[mode];
          const icon = info?.icon ?? UNKNOWN_MODE_ICON;
          const description = info ? t(info.descriptionKey) : mode;
          const isSelected = !showAsPlaceholder && selected === mode;
          const isInherited = inherited === mode;
          return (
            <button
              key={mode}
              type="button"
              onClick={() => onChange(mode)}
              className={cn(
                'flex items-start gap-3 rounded-lg p-3 text-left cursor-pointer transition-colors duration-150',
                isSelected
                  ? 'border-2 border-primary'
                  : isInherited
                    ? 'border-2 border-dashed border-primary/30'
                    : 'border border-input',
                isInherited && !isSelected && 'opacity-50',
              )}
              style={
                isSelected
                  ? {
                      background:
                        'color-mix(in srgb, var(--primary, #4f46e5) 5%, transparent)',
                    }
                  : isInherited
                    ? {
                        background:
                          'color-mix(in srgb, var(--primary, #4f46e5) 3%, transparent)',
                      }
                    : undefined
              }
            >
              <span className="text-xl leading-none mt-0.5">{icon}</span>
              <div>
                <div className="text-[13px] font-medium capitalize">{mode}</div>
                <div className="text-[11px] opacity-60 mt-0.5 leading-snug">
                  {description}
                </div>
              </div>
            </button>
          );
        })}
      </div>
      {inheritedSource && (
        <InheritedAnnotation
          source={inheritedSource}
          value={String(inheritedValue ?? '')}
          isOverride={!showAsPlaceholder}
        />
      )}
    </div>
  );
}
