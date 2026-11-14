import { useApiClient } from '@broccoli/web-sdk/api';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import type { PluginFullDetail, UploadRemoval } from '@broccoli/web-sdk/plugin';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Skeleton,
} from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import {
  AlertCircle,
  Globe,
  Loader2,
  Package,
  RefreshCw,
  Server,
  ServerCog,
} from 'lucide-react';
import { useState } from 'react';

import type { PluginFleet } from '../fleet';
import { PluginSourceBadge } from './PluginCard';
import { PluginFleetBadge } from './PluginFleetBadge';

// -- Section wrapper --

function Section({
  title,
  icon,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-2">
      <h3 className="text-sm font-semibold flex items-center gap-1.5">
        {icon}
        {title}
      </h3>
      {children}
    </div>
  );
}

// -- Mini table --

function MiniTable({
  headers,
  rows,
  emptyMessage,
}: {
  headers: string[];
  rows: (string | React.ReactNode)[][];
  emptyMessage: string;
}) {
  if (rows.length === 0) {
    return (
      <p className="text-xs text-muted-foreground italic">{emptyMessage}</p>
    );
  }

  return (
    <div className="rounded-md border overflow-hidden">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b bg-muted/40">
            {headers.map((h) => (
              <th
                key={h}
                className="px-3 py-1.5 text-left font-medium text-foreground/80"
              >
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr
              key={`row-${String(i)}`}
              className="border-b last:border-0 hover:bg-muted/20"
            >
              {row.map((cell, j) => (
                <td
                  key={`cell-${String(i)}-${String(j)}`}
                  className="px-3 py-1.5"
                >
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// -- Per-server load state --

type ReplicaStatus = PluginFleet['replicas'][number]['status'];

const REPLICA_BADGE: Record<
  ReplicaStatus,
  'secondary' | 'destructive' | 'outline'
> = {
  loaded: 'secondary',
  failed: 'destructive',
  not_installed: 'destructive',
  outdated: 'destructive',
  mismatched: 'destructive',
  unloaded: 'outline',
};

function FleetSection({ fleet }: { fleet: PluginFleet }) {
  const { t } = useTranslation();
  const format = useFormat();

  return (
    <Section
      title={t('plugins.detail.servers')}
      icon={<ServerCog className="h-4 w-4" />}
    >
      {fleet.replicas.length === 0 ? (
        <p className="text-xs text-muted-foreground italic">
          {t('plugins.detail.noServers')}
        </p>
      ) : (
        <ul className="rounded-md border divide-y text-xs">
          {fleet.replicas.map((r) => (
            <li key={r.server_id} className="px-3 py-2 space-y-1">
              <div className="flex items-center gap-2">
                <span className="font-mono">{r.server_id}</span>
                {r.version && (
                  <span className="text-muted-foreground font-mono">
                    {t('common.version', { version: r.version })}
                  </span>
                )}
                <Badge variant={REPLICA_BADGE[r.status]} className="text-xs">
                  {t(`plugins.status.${r.status}`)}
                </Badge>
                {!r.up_to_date && (
                  <span className="text-muted-foreground">
                    {t('plugins.detail.outdated')}
                  </span>
                )}
              </div>
              {r.error && (
                <pre className="whitespace-pre-wrap break-words font-mono text-destructive">
                  {r.error}
                </pre>
              )}
              {r.retry_at && (
                <p className="text-muted-foreground">
                  {t('plugins.detail.retryAt', {
                    time: format.time(r.retry_at),
                  })}
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

// -- Source --

function SourceSection({
  plugin,
  onRemoveUpload,
  removingUpload,
}: {
  plugin: PluginFullDetail;
  onRemoveUpload?: (pluginId: string, expect: UploadRemoval) => void;
  removingUpload: boolean;
}) {
  const { t } = useTranslation();
  const [confirming, setConfirming] = useState(false);
  const { source } = plugin;
  const uploaded = source.kind === 'upload';

  return (
    <Section
      title={t('plugins.detail.source')}
      icon={<Package className="h-4 w-4" />}
    >
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <PluginSourceBadge kind={source.kind} />
        <span className="text-muted-foreground">
          {t(
            uploaded
              ? 'plugins.detail.sourceUpload'
              : 'plugins.detail.sourceBundle',
          )}
        </span>
      </div>
      {source.archive_hash && (
        <p className="font-mono text-[11px] text-muted-foreground break-all">
          {t('plugins.archiveHash', { hash: source.archive_hash })}
        </p>
      )}
      {uploaded && onRemoveUpload && !confirming && (
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs"
          disabled={removingUpload}
          onClick={() => setConfirming(true)}
        >
          {t('plugins.detail.removeUpload')}
        </Button>
      )}
      {uploaded && onRemoveUpload && confirming && (
        <div className="rounded-md border border-destructive/50 p-2 space-y-2">
          <p className="text-xs">
            {t(
              source.bundled
                ? 'plugins.detail.removeUploadToBundle'
                : 'plugins.detail.removeUploadDeletes',
            )}
          </p>
          <div className="flex gap-2">
            <Button
              variant="destructive"
              size="sm"
              className="h-7 text-xs"
              disabled={removingUpload}
              onClick={() => {
                setConfirming(false);
                // The outcome this confirmation described: the server refuses
                // if it would do the other.
                onRemoveUpload(
                  plugin.id,
                  source.bundled ? 'bundle_restored' : 'removed',
                );
              }}
            >
              {removingUpload && (
                <Loader2 className="h-3 w-3 mr-1 animate-spin" />
              )}
              {t('plugins.detail.removeUploadConfirm')}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 text-xs"
              onClick={() => setConfirming(false)}
            >
              {t('plugins.detail.removeUploadCancel')}
            </Button>
          </div>
        </div>
      )}
    </Section>
  );
}

// -- Detail content --

function PluginDetailContent({
  plugin,
  onRemoveUpload,
  removingUpload,
}: {
  plugin: PluginFullDetail;
  onRemoveUpload?: (pluginId: string, expect: UploadRemoval) => void;
  removingUpload: boolean;
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-5">
      <SourceSection
        plugin={plugin}
        onRemoveUpload={onRemoveUpload}
        removingUpload={removingUpload}
      />
      <FleetSection fleet={plugin.fleet} />

      {/* Server section */}
      {plugin.server && (
        <Section
          title={t('plugins.detail.server')}
          icon={<Server className="h-4 w-4" />}
        >
          {plugin.server.permissions.length > 0 && (
            <div className="space-y-1">
              <p className="text-xs font-medium text-muted-foreground">
                {t('plugins.detail.permissions')}
              </p>
              <div className="flex flex-wrap gap-1">
                {plugin.server.permissions.map((p) => (
                  <Badge key={p} variant="secondary" className="text-xs">
                    {p}
                  </Badge>
                ))}
              </div>
            </div>
          )}
          {plugin.server.sql_read.length > 0 && (
            <div className="space-y-1">
              <p className="text-xs font-medium text-muted-foreground">
                {t('plugins.detail.sqlRead')}
              </p>
              <div className="flex flex-wrap gap-1">
                {plugin.server.sql_read.map((scope) => (
                  <Badge
                    key={scope}
                    variant="secondary"
                    className="font-mono text-xs"
                  >
                    {scope}
                  </Badge>
                ))}
              </div>
            </div>
          )}
          <MiniTable
            headers={[
              t('plugins.detail.method'),
              t('plugins.detail.path'),
              t('plugins.detail.handler'),
              t('plugins.detail.permission'),
            ]}
            rows={plugin.server.routes.map((r) => [
              r.method,
              r.path,
              r.handler,
              r.permission ?? '—',
            ])}
            emptyMessage={t('plugins.detail.noRoutes')}
          />
        </Section>
      )}

      {/* Web section */}
      {plugin.web && (
        <Section
          title={t('plugins.detail.web')}
          icon={<Globe className="h-4 w-4" />}
        >
          {/* Components */}
          <p className="text-xs font-medium text-muted-foreground">
            {t('plugins.detail.components')}
          </p>
          <MiniTable
            headers={[
              t('plugins.detail.component'),
              t('plugins.detail.export'),
            ]}
            rows={Object.entries(plugin.web.components).map(([name, exp]) => [
              name,
              exp,
            ])}
            emptyMessage={t('plugins.detail.noComponents')}
          />

          {/* Slots */}
          {plugin.web.slots.length > 0 && (
            <>
              <p className="text-xs font-medium text-muted-foreground mt-3">
                {t('plugins.detail.slots')}
              </p>
              <MiniTable
                headers={[
                  t('plugins.detail.slotName'),
                  t('plugins.detail.position'),
                  t('plugins.detail.component'),
                  t('plugins.detail.priority'),
                ]}
                rows={plugin.web.slots.map((s) => [
                  s.name,
                  s.position,
                  s.component,
                  s.priority != null ? String(s.priority) : '—',
                ])}
                emptyMessage={t('plugins.detail.noSlots')}
              />
            </>
          )}

          {/* Web routes */}
          {plugin.web.routes.length > 0 && (
            <>
              <p className="text-xs font-medium text-muted-foreground mt-3">
                {t('plugins.detail.routes')}
              </p>
              <MiniTable
                headers={[
                  t('plugins.detail.path'),
                  t('plugins.detail.component'),
                  t('plugins.detail.meta'),
                ]}
                rows={plugin.web.routes.map((r) => [
                  r.path,
                  r.component,
                  r.meta ? JSON.stringify(r.meta) : '—',
                ])}
                emptyMessage={t('plugins.detail.noRoutes')}
              />
            </>
          )}
        </Section>
      )}

      {/* Translations section */}
      {plugin.translations && plugin.translations.length > 0 && (
        <Section
          title={t('plugins.detail.translations')}
          icon={<Globe className="h-4 w-4" />}
        >
          <div className="flex flex-wrap gap-1">
            {plugin.translations?.map((locale) => (
              <Badge key={locale} variant="secondary" className="text-xs">
                {locale}
              </Badge>
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}

// -- Main dialog --

export function PluginDetailDialog({
  pluginId,
  open,
  onOpenChange,
  onRetry,
  retrying = false,
  onRemoveUpload,
  removingUpload = false,
}: {
  pluginId: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Reload the plugin on every server (the retry for a failed load). */
  onRetry?: (pluginId: string) => void;
  retrying?: boolean;
  /** Remove the plugin's uploaded version on every server. */
  onRemoveUpload?: (pluginId: string, expect: UploadRemoval) => void;
  removingUpload?: boolean;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();

  const {
    data: plugin,
    isLoading,
    error,
  } = useQuery({
    queryKey: ['admin-plugin-detail', pluginId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/admin/plugins/{id}', {
        params: { path: { id: pluginId! } },
      });
      if (error) throw error;
      return data;
    },
    enabled: open && !!pluginId,
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {plugin ? plugin.name : t('plugins.detail.title')}
          </DialogTitle>
          <DialogDescription asChild>
            <div className="text-sm text-muted-foreground">
              {plugin && (
                <span className="flex flex-wrap items-center gap-2">
                  <span className="font-mono text-xs">
                    {plugin.id}
                    {plugin.version &&
                      ` · ${t('common.version', { version: plugin.version })}`}
                  </span>
                  <PluginFleetBadge fleet={plugin.fleet} />
                  {plugin.enabled &&
                    plugin.fleet.state === 'failed' &&
                    onRetry && (
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-6 text-xs"
                        disabled={retrying}
                        onClick={() => onRetry(plugin.id)}
                      >
                        {retrying ? (
                          <Loader2 className="h-3 w-3 mr-1 animate-spin" />
                        ) : (
                          <RefreshCw className="h-3 w-3 mr-1" />
                        )}
                        {t('plugins.retry')}
                      </Button>
                    )}
                </span>
              )}
            </div>
          </DialogDescription>
        </DialogHeader>

        {isLoading && (
          <div className="space-y-4 py-4">
            <Skeleton className="h-4 w-3/4" />
            <Skeleton className="h-20 w-full" />
            <Skeleton className="h-4 w-1/2" />
            <Skeleton className="h-16 w-full" />
          </div>
        )}

        {error && (
          <div className="py-6 text-center">
            <AlertCircle className="mx-auto h-8 w-8 text-destructive mb-2" />
            <p className="text-sm text-destructive">
              {t('plugins.detail.loadError')}
            </p>
          </div>
        )}

        {plugin && (
          <PluginDetailContent
            plugin={plugin}
            onRemoveUpload={onRemoveUpload}
            removingUpload={removingUpload}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}
