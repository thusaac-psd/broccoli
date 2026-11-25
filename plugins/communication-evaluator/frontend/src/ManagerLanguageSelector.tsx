import {
  loadedData,
  registryChoiceLabel,
  registryChoices,
  useQueryState,
  useRegistries,
} from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedAnnotation, InheritedBadge } from '@broccoli/web-sdk/slot';
import {
  Label,
  QueryErrorState,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@broccoli/web-sdk/ui';

export function ManagerLanguageSelector({
  value,
  label,
  description,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const registries = useQueryState(useRegistries());
  const languages = loadedData(registries)?.languages;
  const isLoading = registries.status === 'loading';

  const selectedValue = showAsPlaceholder
    ? ''
    : typeof value === 'string'
      ? value
      : '';
  const inheritedDisplay =
    showAsPlaceholder && typeof inheritedValue === 'string'
      ? inheritedValue
      : undefined;

  // A stored language this server does not register stays chosen, shown as
  // unavailable.
  const choices = registryChoices(
    languages?.map((lang) => lang.id),
    selectedValue,
  );
  const nameOf = (id: string) =>
    languages?.find((lang) => lang.id === id)?.name ?? id;

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-semibold uppercase tracking-wider opacity-55">
          {label}
        </Label>
        {showAsPlaceholder && inheritedSource && (
          <InheritedBadge source={inheritedSource} />
        )}
      </div>
      {description && (
        <p className="text-xs opacity-50 m-0 leading-normal">{description}</p>
      )}

      <Select
        value={selectedValue || undefined}
        onValueChange={(v) => onChange(v)}
        disabled={languages === undefined}
      >
        <SelectTrigger aria-label={label}>
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
          {choices.map((choice) => (
            <SelectItem key={choice.value} value={choice.value}>
              {registryChoiceLabel(choice, nameOf(choice.value), t)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {registries.status === 'error' && (
        <QueryErrorState state={registries} className="py-4" />
      )}
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
