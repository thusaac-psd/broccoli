/**
 * Replaces the plain text input for `accent_color` with a native color picker
 * plus a text input showing the hex value, and a live preview swatch.
 *
 * Receives slot props: { value, schema, onChange, showAsPlaceholder, inheritedValue, inheritedSource, ... }
 */

import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedAnnotation, InheritedBadge } from '@broccoli/web-sdk/slot';
import { Input, Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';

export function ColorPickerField({
  value,
  schema,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const displayColor = showAsPlaceholder
    ? typeof inheritedValue === 'string'
      ? inheritedValue
      : '#000000'
    : typeof value === 'string'
      ? value
      : '#000000';

  return (
    <div className="flex flex-col gap-1.5 col-span-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-medium uppercase tracking-wide opacity-60">
          {schema.title ?? t('configUiTest.colorPicker.defaultTitle')}
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
      <div className="flex items-center gap-3">
        {/* Native color picker */}
        <input
          type="color"
          value={displayColor}
          onChange={(e) => onChange(e.target.value)}
          className={cn(
            'h-9 w-12 cursor-pointer rounded-md border border-input p-0.5',
            showAsPlaceholder && 'opacity-40',
          )}
        />
        {/* Hex text input */}
        <Input
          type="text"
          value={showAsPlaceholder ? '' : displayColor}
          placeholder={
            showAsPlaceholder && inheritedValue
              ? String(inheritedValue)
              : undefined
          }
          onChange={(e) => onChange(e.target.value)}
          maxLength={20}
          className="w-28 font-mono"
        />
        {/* Live preview swatch */}
        <div className="flex items-center gap-2">
          <div
            className={cn(
              'h-9 w-9 rounded-md border border-input',
              showAsPlaceholder && 'opacity-40',
            )}
            style={{ backgroundColor: displayColor }}
          />
          <span className="text-xs opacity-60">
            {t('configUiTest.colorPicker.preview')}
          </span>
        </div>
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
