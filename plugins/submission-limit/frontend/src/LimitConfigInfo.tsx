/**
 * Config inheritance display for the submission-limit plugin.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { ConfigInheritanceInfo } from '@broccoli/web-sdk/plugin';
import type { ConfigFormSlotProps } from '@broccoli/web-sdk/slot';

export function LimitConfigInfo(props: ConfigFormSlotProps) {
  const { t } = useTranslation();

  return (
    <ConfigInheritanceInfo
      {...props}
      fieldKey="max_submissions"
      formatValue={(v) => (v === 0 ? t('limit.unlimited') : String(v))}
      labels={{
        contest: t('limit.sourceContest'),
        problem: t('limit.sourceProblem'),
        contestHint: t('limit.contestHint'),
        inheritInfo: (value, source) =>
          t('limit.inheritInfo', { value, source }),
        overrideInfo: t('limit.overrideInfo'),
        notSet: t('limit.notSet'),
        notSetWithDefault: (def) =>
          t('limit.notSetWithDefault', { default: def }),
        active: t('limit.active'),
      }}
    />
  );
}
