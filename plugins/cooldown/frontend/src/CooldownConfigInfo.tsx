/**
 * Config inheritance display for the cooldown plugin.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { ConfigInheritanceInfo } from '@broccoli/web-sdk/plugin';
import type { ConfigFormSlotProps } from '@broccoli/web-sdk/slot';

export function CooldownConfigInfo(props: ConfigFormSlotProps) {
  const { t } = useTranslation();

  return (
    <ConfigInheritanceInfo
      {...props}
      fieldKey="cooldown_seconds"
      formatValue={(v) =>
        v === 0
          ? t('cooldown.disabled')
          : t('cooldown.secondsValue', { seconds: v as number })
      }
      labels={{
        contest: t('cooldown.contest'),
        problem: t('cooldown.problem'),
        contestHint: t('cooldown.contestScopeHint'),
        inheritInfo: (value, source) =>
          t('cooldown.inheritInfo', { value, source }),
        overrideInfo: t('cooldown.overrideInfo'),
        notSet: t('cooldown.notSet'),
        notSetWithDefault: (def) =>
          t('cooldown.notSetWithDefault', { default: def }),
        active: t('cooldown.active'),
      }}
    />
  );
}
