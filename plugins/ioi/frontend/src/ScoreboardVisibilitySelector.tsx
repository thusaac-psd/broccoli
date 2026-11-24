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

import type { ScoreboardVisibility } from './types';

const VISIBILITY_OPTIONS: ReadonlyArray<{
  value: ScoreboardVisibility;
  labelKey: string;
  descriptionKey: string;
}> = [
  {
    value: 'admins_only',
    labelKey: 'ioi.scoreboardVisibility.adminsOnly.label',
    descriptionKey: 'ioi.scoreboardVisibility.adminsOnly.description',
  },
  {
    value: 'all_contest_viewers',
    labelKey: 'ioi.scoreboardVisibility.allContestViewers.label',
    descriptionKey: 'ioi.scoreboardVisibility.allContestViewers.description',
  },
];

function isScoreboardVisibility(value: unknown): value is ScoreboardVisibility {
  return value === 'admins_only' || value === 'all_contest_viewers';
}

export function ScoreboardVisibilitySelector({
  value,
  label,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const selectedValue =
    !showAsPlaceholder && isScoreboardVisibility(value) ? value : 'admins_only';
  const inheritedDisplay =
    showAsPlaceholder && isScoreboardVisibility(inheritedValue)
      ? t(
          VISIBILITY_OPTIONS.find((option) => option.value === inheritedValue)
            ?.labelKey ?? 'ioi.scoreboardVisibility.adminsOnly.label',
        )
      : undefined;

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
      <p className="text-xs opacity-50 m-0 leading-normal">
        {t('ioi.scoreboardVisibility.description')}
      </p>

      <Select value={selectedValue} onValueChange={(v) => onChange(v)}>
        <SelectTrigger aria-label={label}>
          <SelectValue placeholder={inheritedDisplay ?? label} />
        </SelectTrigger>
        <SelectContent>
          {VISIBILITY_OPTIONS.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {t(option.labelKey)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <p className="text-xs m-0 text-muted-foreground leading-normal">
        {t(
          VISIBILITY_OPTIONS.find((option) => option.value === selectedValue)
            ?.descriptionKey ??
            'ioi.scoreboardVisibility.adminsOnly.description',
        )}
      </p>
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
