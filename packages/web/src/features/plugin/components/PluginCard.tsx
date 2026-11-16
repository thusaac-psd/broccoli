import {
  pluginKeys,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import type { PluginDetail } from '@broccoli/web-sdk/plugin';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Switch,
} from '@broccoli/web-sdk/ui';
import {
  Globe,
  Loader2,
  Package,
  RefreshCw,
  Server,
  Settings,
  Upload,
} from 'lucide-react';
import { useState } from 'react';

import { ResourceConfigDialog } from '@/components/config';

import { firstFleetError } from '../fleet';
import { PluginFleetBadge } from './PluginFleetBadge';

export function PluginCard({
  plugin,
  toggling,
  reloading = false,
  onToggle,
  onReload,
  onClick,
}: {
  plugin: PluginDetail;
  toggling: boolean;
  reloading?: boolean;
  onToggle: (plugin: PluginDetail, enable: boolean) => void;
  onReload?: (plugin: PluginDetail) => void;
  onClick?: (plugin: PluginDetail) => void;
}) {
  const { t } = useTranslation();
  const pluginText = usePluginText();
  const [configOpen, setConfigOpen] = useState(false);
  // The fleet-wide desired state, not whether the server that answered has
  // it loaded: a plugin enabled but failing to load is still enabled.
  const isEnabled = plugin.enabled;
  const failed = plugin.fleet.state === 'failed';
  const hasPluginSchemas = plugin.config_schemas?.some((schema) =>
    schema.scopes.includes('plugin'),
  );

  return (
    <>
      <Card
        className={`flex flex-col${onClick ? ' cursor-pointer hover:bg-muted/50' : ''}`}
        onClick={() => onClick?.(plugin)}
      >
        <CardHeader>
          <CardTitle className="text-base flex items-center gap-2">
            <span className="truncate">
              {pluginText(pluginKeys.name(plugin.id), plugin.name)}
            </span>
          </CardTitle>
          <CardDescription className="text-xs font-mono">
            {plugin.id}
            {plugin.version && (
              <span className="ml-2 text-muted-foreground">
                {t('common.version', { version: plugin.version })}
              </span>
            )}
          </CardDescription>
        </CardHeader>

        <CardContent className="flex-1">
          {plugin.description && (
            <p className="text-sm text-muted-foreground mb-3">
              {pluginText(
                pluginKeys.description(plugin.id),
                plugin.description,
              )}
            </p>
          )}
          {failed && (
            <p
              className="text-xs text-destructive mb-3 line-clamp-2 break-words"
              title={firstFleetError(plugin.fleet)}
            >
              {firstFleetError(plugin.fleet) ?? t('plugins.fleet.notInstalled')}
            </p>
          )}
          <div className="flex items-end justify-between gap-4">
            <div className="flex flex-wrap gap-2">
              <PluginFleetBadge fleet={plugin.fleet} />
              <PluginSourceBadge kind={plugin.source.kind} />
              {plugin.has_server && (
                <Badge variant="outline" className="gap-1 text-xs">
                  <Server className="h-3 w-3" />
                  {t('plugins.component.server')}
                </Badge>
              )}
              {plugin.has_web && (
                <Badge variant="outline" className="gap-1 text-xs">
                  <Globe className="h-3 w-3" />
                  {t('plugins.component.web')}
                </Badge>
              )}
            </div>
            <div
              className="flex items-center gap-2 shrink-0"
              onClick={(e) => e.stopPropagation()}
            >
              {hasPluginSchemas && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => setConfigOpen(true)}
                  aria-label={t('plugins.configure')}
                  title={t('plugins.configure')}
                >
                  <Settings className="h-4 w-4" />
                </Button>
              )}
              {isEnabled && onReload && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => onReload(plugin)}
                  disabled={reloading || toggling}
                  aria-label={failed ? t('plugins.retry') : t('plugins.reload')}
                  title={failed ? t('plugins.retry') : t('plugins.reload')}
                >
                  {reloading ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <RefreshCw className="h-4 w-4" />
                  )}
                </Button>
              )}
              {toggling && (
                <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
              )}
              <Switch
                checked={isEnabled}
                disabled={toggling || reloading}
                onCheckedChange={(checked) => onToggle(plugin, checked)}
                aria-label={
                  isEnabled ? t('plugins.disable') : t('plugins.enable')
                }
              />
            </div>
          </div>
        </CardContent>
      </Card>
      {hasPluginSchemas && (
        <ResourceConfigDialog
          scope={{ scope: 'plugin', pluginId: plugin.id }}
          resourceLabel={plugin.name}
          open={configOpen}
          onOpenChange={setConfigOpen}
        />
      )}
    </>
  );
}

/** Where the fleet takes the plugin's files from. */
export function PluginSourceBadge({
  kind,
}: {
  kind: PluginDetail['source']['kind'];
}) {
  const { t } = useTranslation();
  const Icon = kind === 'upload' ? Upload : Package;
  return (
    <Badge variant="outline" className="gap-1 text-xs">
      <Icon className="h-3 w-3" />
      {t(`plugins.source.${kind}`)}
    </Badge>
  );
}
