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
      formatValue={(v) =>
        v === 0 ? t('submission-limit.unlimited') : String(v)
      }
      labels={{
        contest: t('submission-limit.sourceContest'),
        problem: t('submission-limit.sourceProblem'),
        contestHint: t('submission-limit.contestHint'),
        inheritInfo: (value, source) =>
          t('submission-limit.inheritInfo', { value, source }),
        overrideInfo: t('submission-limit.overrideInfo'),
        notSet: t('submission-limit.notSet'),
        notSetWithDefault: (def) =>
          t('submission-limit.notSetWithDefault', { default: def }),
        active: t('submission-limit.active'),
      }}
    />
  );
}
