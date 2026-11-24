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

import { getConfiguredTokenMode } from './config-rules';
import type { FeedbackLevel } from './types';

const FEEDBACK_OPTIONS: ReadonlyArray<{
  value: FeedbackLevel;
  labelKey: string;
}> = [
  { value: 'full', labelKey: 'ioi.contestInfo.feedback.full' },
  {
    value: 'subtask_scores',
    labelKey: 'ioi.contestInfo.feedback.subtaskScores',
  },
  { value: 'total_only', labelKey: 'ioi.contestInfo.feedback.totalOnly' },
  { value: 'none', labelKey: 'ioi.contestInfo.feedback.none' },
];

export function FeedbackLevelSelector({
  value,
  label,
  description,
  onChange,
  formValues,
  showAsPlaceholder,
  inheritedValue,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const tokenMode = getConfiguredTokenMode(formValues);
  const tokenEnabled = tokenMode !== 'none';
  const selectedValue = showAsPlaceholder
    ? undefined
    : typeof value === 'string' &&
        FEEDBACK_OPTIONS.some((option) => option.value === value)
      ? value
      : 'full';
  const inheritedDisplay =
    showAsPlaceholder && typeof inheritedValue === 'string'
      ? inheritedValue
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
      {description && (
        <p className="text-xs opacity-50 m-0 leading-normal">{description}</p>
      )}

      <Select value={selectedValue} onValueChange={(v) => onChange(v)}>
        <SelectTrigger aria-label={label}>
          <SelectValue placeholder={inheritedDisplay ?? label} />
        </SelectTrigger>
        <SelectContent>
          {FEEDBACK_OPTIONS.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {t(option.labelKey)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <p className="text-xs m-0 text-muted-foreground leading-normal">
        {tokenEnabled
          ? t('ioi.feedbackLevel.tokenEnabledHint')
          : t('ioi.feedbackLevel.tokenDisabledHint')}
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
