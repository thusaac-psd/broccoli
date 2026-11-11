import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { AlertTriangle, Loader2 } from 'lucide-react';

import { firstFleetError, type PluginFleet } from '../fleet';

/**
 * The plugin's state across all servers, when it needs attention: failing
 * somewhere, or still being applied. Nothing when it is loaded everywhere or
 * disabled everywhere (the enable switch already says which).
 */
export function PluginFleetBadge({ fleet }: { fleet: PluginFleet }) {
  const { t } = useTranslation();

  if (fleet.state === 'failed') {
    return (
      <Badge
        variant="destructive"
        className="gap-1 text-xs"
        title={firstFleetError(fleet)}
      >
        <AlertTriangle className="h-3 w-3" />
        {t('plugins.fleet.failedOn', {
          failed: fleet.replicas_failed,
          total: fleet.replicas_total,
        })}
      </Badge>
    );
  }

  if (fleet.state === 'pending') {
    return (
      <Badge variant="secondary" className="gap-1 text-xs">
        <Loader2 className="h-3 w-3 animate-spin" />
        {t('plugins.fleet.pending')}
      </Badge>
    );
  }

  return null;
}
