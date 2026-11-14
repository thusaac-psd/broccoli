import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { PLUGIN_MANAGE } from '@broccoli/web-sdk/permissions';
import type { PluginDetail, UploadRemoval } from '@broccoli/web-sdk/plugin';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import {
  Button,
  Card,
  CardContent,
  CardHeader,
  QueryErrorState,
  Skeleton,
} from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Loader2, Puzzle, RefreshCw, Upload } from 'lucide-react';
import { useCallback, useState } from 'react';
import { useSearchParams } from 'react-router';
import { toast } from 'sonner';

import { PageLayout } from '@/components/PageLayout';
import { Unauthorized } from '@/components/Unauthorized';
import { PluginCard } from '@/features/plugin/components/PluginCard';
import { PluginDetailDialog } from '@/features/plugin/components/PluginDetailDialog';
import { PluginUploadDialog } from '@/features/plugin/components/PluginUploadDialog';

export default function PluginsPage() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const { user } = useAuth();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();

  const [togglingIds, setTogglingIds] = useState<Set<string>>(() => new Set());
  const [reloadingIds, setReloadingIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [isReloadingAll, setIsReloadingAll] = useState(false);
  const [uploadOpen, setUploadOpen] = useState(false);
  const [removingUploadId, setRemovingUploadId] = useState<string | null>(null);
  const detailPluginId = searchParams.get('plugin');
  const { unloadPlugin, reloadPlugin, reloadAllPlugins } = usePluginRegistry();

  const setDetailPluginId = useCallback(
    (pluginId: string | null) => {
      const nextParams = new URLSearchParams(searchParams);

      if (pluginId) {
        nextParams.set('plugin', pluginId);
      } else {
        nextParams.delete('plugin');
      }

      setSearchParams(nextParams);
    },
    [searchParams, setSearchParams],
  );

  // Gate the fetch on the same permission as the render guard below: the hook
  // runs before the early Unauthorized return, so without this an unauthorized
  // viewer would 403 on /admin/plugins.
  const canManagePlugins = !!user?.permissions.includes(PLUGIN_MANAGE);

  const pluginsQuery = useQuery({
    queryKey: ['admin-plugins'],
    enabled: canManagePlugins,
    // Other servers apply a change within a few seconds: keep polling while
    // any plugin is still converging, so the page settles on its own.
    refetchInterval: (query) =>
      query.state.data?.some((p) => p.fleet.state === 'pending') ? 3000 : false,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/admin/plugins');
      if (error) throw error;
      return data;
    },
  });
  const { data: plugins, isLoading, error } = pluginsQuery;

  const { data: uploadPolicy } = useQuery({
    queryKey: ['admin-plugin-upload-policy'],
    enabled: canManagePlugins,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/admin/plugins/upload');
      if (error) throw error;
      return data;
    },
  });

  const invalidatePluginQueries = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ['admin-plugins'] });
    queryClient.invalidateQueries({ queryKey: ['admin-plugin-detail'] });
    queryClient.invalidateQueries({ queryKey: ['i18n'] });
  }, [queryClient]);

  const handleToggle = useCallback(
    async (plugin: PluginDetail, enable: boolean) => {
      setTogglingIds((prev) => new Set(prev).add(plugin.id));
      try {
        const endpoint = enable
          ? '/admin/plugins/{id}/enable'
          : '/admin/plugins/{id}/disable';

        const { error } = await apiClient.POST(endpoint, {
          params: { path: { id: plugin.id } },
        });

        // Refresh either way: a rejected toggle may mean the page was stale.
        invalidatePluginQueries();
        if (error) {
          toast.error(errorMessage(error, t('validation.pluginToggleError')));
        } else {
          // Unnecessary to load the plugin immediately, as it will be lazily
          // loaded when the user navigates to a page that uses it.
          if (!enable && plugin.has_web) {
            unloadPlugin(plugin.id);
          }
        }
      } finally {
        setTogglingIds((prev) => {
          const next = new Set(prev);
          next.delete(plugin.id);
          return next;
        });
      }
    },
    [errorMessage, apiClient, invalidatePluginQueries, unloadPlugin, t],
  );

  // Reload on every server. Also the retry for a plugin that failed to load:
  // it is accepted while the plugin is enabled, wherever it failed.
  const handleReload = useCallback(
    async (pluginId: string) => {
      setReloadingIds((prev) => new Set(prev).add(pluginId));
      try {
        const { error } = await apiClient.POST('/admin/plugins/{id}/reload', {
          params: { path: { id: pluginId } },
        });

        if (error) {
          toast.error(errorMessage(error, t('plugins.reloadError')));
        } else {
          await reloadPlugin(pluginId);
        }
        // Refresh either way: a failed reload changes the reported state too.
        invalidatePluginQueries();
      } finally {
        setReloadingIds((prev) => {
          const next = new Set(prev);
          next.delete(pluginId);
          return next;
        });
      }
    },
    [errorMessage, apiClient, invalidatePluginQueries, reloadPlugin, t],
  );

  const handleReloadAll = useCallback(async () => {
    setIsReloadingAll(true);
    try {
      const { error } = await apiClient.POST('/admin/plugins/reload-all', {});

      if (error) {
        toast.error(errorMessage(error, t('plugins.reloadError')));
      } else {
        await reloadAllPlugins();
      }
      invalidatePluginQueries();
    } finally {
      setIsReloadingAll(false);
    }
  }, [errorMessage, apiClient, invalidatePluginQueries, reloadAllPlugins, t]);

  const handlePublished = useCallback(
    async (pluginId: string) => {
      invalidatePluginQueries();
      // Pick up the new version's web bundle, if it has one.
      await reloadPlugin(pluginId);
    },
    [invalidatePluginQueries, reloadPlugin],
  );

  const handleRemoveUpload = useCallback(
    async (pluginId: string, expect: UploadRemoval) => {
      setRemovingUploadId(pluginId);
      try {
        const { data, error } = await apiClient.DELETE(
          '/admin/plugins/{id}/upload',
          { params: { path: { id: pluginId }, query: { expect } } },
        );
        if (error || !data) {
          toast.error(
            errorMessage(error, t('plugins.detail.removeUploadError')),
          );
        } else {
          toast.success(
            t(
              data.outcome === 'removed'
                ? 'plugins.detail.uploadRemoved'
                : 'plugins.detail.uploadReplacedByBundle',
            ),
          );
          if (data.outcome === 'removed') {
            unloadPlugin(pluginId);
            setDetailPluginId(null);
          } else {
            await reloadPlugin(pluginId);
          }
        }
        invalidatePluginQueries();
      } finally {
        setRemovingUploadId(null);
      }
    },
    [
      errorMessage,
      apiClient,
      invalidatePluginQueries,
      reloadPlugin,
      setDetailPluginId,
      t,
      unloadPlugin,
    ],
  );

  if (!canManagePlugins) {
    return <Unauthorized />;
  }

  return (
    <PageLayout
      pageId="plugins"
      icon={<Puzzle className="h-6 w-6 text-primary" />}
      title={t('plugins.title')}
      subtitle={t('plugins.subtitle')}
      actions={
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setUploadOpen(true)}
            disabled={!uploadPolicy?.enabled}
            title={
              uploadPolicy && !uploadPolicy.enabled
                ? t('plugins.upload.disabled')
                : undefined
            }
          >
            <Upload className="h-4 w-4 mr-2" />
            {t('plugins.upload.button')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handleReloadAll}
            disabled={isReloadingAll || isLoading}
          >
            {isReloadingAll ? (
              <Loader2 className="h-4 w-4 animate-spin mr-2" />
            ) : (
              <RefreshCw className="h-4 w-4 mr-2" />
            )}
            {t('plugins.reloadAll')}
          </Button>
        </div>
      }
    >
      {isLoading && (
        <div className="grid gap-4 md:grid-cols-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <Card key={`skeleton-${String(i)}`}>
              <CardHeader>
                <Skeleton className="h-5 w-32" />
                <Skeleton className="h-4 w-48 mt-1" />
              </CardHeader>
              <CardContent>
                <Skeleton className="h-4 w-full" />
                <Skeleton className="h-4 w-3/4 mt-2" />
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {error && !plugins && (
        <QueryErrorState
          error={error}
          retry={() => void pluginsQuery.refetch()}
          retrying={pluginsQuery.isFetching}
          failure={t('plugins.loadError')}
        />
      )}

      {plugins && plugins.length === 0 && (
        <Card>
          <CardContent className="pt-6 text-center">
            <Puzzle className="mx-auto h-10 w-10 text-muted-foreground mb-2" />
            <p className="text-muted-foreground">{t('plugins.empty')}</p>
          </CardContent>
        </Card>
      )}

      {plugins && plugins.length > 0 && (
        <div className="grid gap-4 md:grid-cols-2">
          {plugins.map((plugin) => (
            <PluginCard
              key={plugin.id}
              plugin={plugin}
              toggling={togglingIds.has(plugin.id)}
              reloading={reloadingIds.has(plugin.id)}
              onToggle={handleToggle}
              onReload={(p) => handleReload(p.id)}
              onClick={(p) => setDetailPluginId(p.id)}
            />
          ))}
        </div>
      )}
      <PluginDetailDialog
        pluginId={detailPluginId}
        open={!!detailPluginId}
        onRetry={handleReload}
        retrying={!!detailPluginId && reloadingIds.has(detailPluginId)}
        onRemoveUpload={handleRemoveUpload}
        removingUpload={!!detailPluginId && removingUploadId === detailPluginId}
        onOpenChange={(open) => {
          if (!open) {
            setDetailPluginId(null);
          }
        }}
      />
      <PluginUploadDialog
        open={uploadOpen}
        onOpenChange={setUploadOpen}
        onPublished={handlePublished}
      />
    </PageLayout>
  );
}
