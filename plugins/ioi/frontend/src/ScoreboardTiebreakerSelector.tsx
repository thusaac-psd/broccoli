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

import type { ScoreboardTiebreaker } from './types';

const TIEBREAKER_OPTIONS: ReadonlyArray<{
  value: ScoreboardTiebreaker;
  labelKey: string;
  descriptionKey: string;
}> = [
  {
    value: 'equal_rank',
    labelKey: 'ioi.scoreboardTiebreaker.equalRank.label',
    descriptionKey: 'ioi.scoreboardTiebreaker.equalRank.description',
  },
  {
    value: 'sum_score_time',
    labelKey: 'ioi.scoreboardTiebreaker.sumScoreTime.label',
    descriptionKey: 'ioi.scoreboardTiebreaker.sumScoreTime.description',
  },
  {
    value: 'max_score_time',
    labelKey: 'ioi.scoreboardTiebreaker.maxScoreTime.label',
    descriptionKey: 'ioi.scoreboardTiebreaker.maxScoreTime.description',
  },
];

function isScoreboardTiebreaker(value: unknown): value is ScoreboardTiebreaker {
  return (
    value === 'equal_rank' ||
    value === 'sum_score_time' ||
    value === 'max_score_time'
  );
}

export function ScoreboardTiebreakerSelector({
  value,
  schema,
  onChange,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const selectedValue =
    !showAsPlaceholder && isScoreboardTiebreaker(value)
      ? value
      : 'max_score_time';
  const inheritedDisplay =
    showAsPlaceholder && isScoreboardTiebreaker(inheritedValue)
      ? t(
          TIEBREAKER_OPTIONS.find((option) => option.value === inheritedValue)
            ?.labelKey ?? 'ioi.scoreboardTiebreaker.maxScoreTime.label',
        )
      : undefined;

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <Label className="text-[11px] font-semibold uppercase tracking-wider opacity-55">
          {t('ioi.scoreboardTiebreaker.label')}
        </Label>
        {showAsPlaceholder && inheritedSource && (
          <InheritedBadge source={inheritedSource} />
        )}
      </div>
      <p className="text-xs opacity-50 m-0 leading-normal">
        {t('ioi.scoreboardTiebreaker.description')}
      </p>

      <Select value={selectedValue} onValueChange={(v) => onChange(v)}>
        <SelectTrigger aria-label={t('ioi.scoreboardTiebreaker.label')}>
          <SelectValue
            placeholder={
              inheritedDisplay ??
              schema.title ??
              t('ioi.scoreboardTiebreaker.label')
            }
          />
        </SelectTrigger>
        <SelectContent>
          {TIEBREAKER_OPTIONS.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {t(option.labelKey)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <p className="text-xs m-0 text-muted-foreground leading-normal">
        {t(
          TIEBREAKER_OPTIONS.find((option) => option.value === selectedValue)
            ?.descriptionKey ??
            'ioi.scoreboardTiebreaker.maxScoreTime.description',
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
