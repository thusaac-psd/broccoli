import { useRegistries } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedAnnotation, InheritedBadge } from '@broccoli/web-sdk/slot';
import {
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@broccoli/web-sdk/ui';

export function ManagerLanguageSelector({
  value,
  schema,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const { data, isLoading } = useRegistries();
  const languages = data?.languages ?? [];

  const selectedValue = showAsPlaceholder
    ? ''
    : typeof value === 'string'
      ? value
      : '';
  const inheritedDisplay =
    showAsPlaceholder && typeof inheritedValue === 'string'
      ? inheritedValue
      : undefined;

  // If current value is not in the list, show it as fallback to prevent data loss
  const hasCurrentValue =
    selectedValue !== '' && languages.some((lang) => lang.id === selectedValue);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-semibold uppercase tracking-wider opacity-55">
          {schema.title ?? t('admin.additionalFiles.language')}
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

      <Select
        value={selectedValue || undefined}
        onValueChange={(v) => onChange(v)}
        disabled={isLoading}
      >
        <SelectTrigger
          aria-label={schema.title ?? t('admin.additionalFiles.language')}
        >
          <SelectValue
            placeholder={
              inheritedDisplay
                ? inheritedDisplay
                : isLoading
                  ? t('admin.loading')
                  : t('admin.submissionFormat.language')
            }
          />
        </SelectTrigger>
        <SelectContent>
          {languages.map((lang) => (
            <SelectItem key={lang.id} value={lang.id}>
              {lang.name}
            </SelectItem>
          ))}
          {selectedValue && !hasCurrentValue && !isLoading && (
            <SelectItem value={selectedValue}>{selectedValue}</SelectItem>
          )}
        </SelectContent>
      </Select>
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
