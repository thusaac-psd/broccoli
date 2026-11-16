import { useQueryClient } from '@tanstack/react-query';
import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
} from 'react';

import { useApiClient } from '@/api/use-api-client';
import {
  createPluginRegistry,
  registryView,
} from '@/plugin/registry/plugin-loads';
import { PluginRegistryContext } from '@/plugin/registry/plugin-registry-context';
import type {
  ActivePluginManifest,
  PluginModule,
  SlotConfig,
} from '@/plugin/types';
import { backoffDelay, type BackoffPolicy } from '@/utils/backoff';
import { retryLoad } from '@/utils/chunk-load';

// While the server cannot be asked for its plugin list, or a plugin it lists
// would not load (a gateway 502 while the server restarts), sync again with
// backoff, so the page does not stay without its plugin views until a manual
// reload.
const SYNC_RETRY: BackoffPolicy = { baseMs: 1_000, maxMs: 60_000 };

/** Tries per plugin load, the first included. */
const PLUGIN_LOAD_ATTEMPTS = 4;

interface PluginRegistryProviderProps {
  children: ReactNode;
  backendUrl: string;
}

/** A unique query string, so a fresh load bypasses the HTTP and module caches. */
function cacheBuster(): string {
  return Math.random().toString(36).slice(2);
}

/**
 * Point the plugin's stylesheet links at `resolve(file)`, adding those that
 * are missing. Links are matched by plugin and file, not by href, which
 * carries a cache buster after a reload.
 */
function injectPluginCss(
  manifest: ActivePluginManifest,
  resolve: (file: string) => string,
) {
  for (const file of manifest.css ?? []) {
    const href = resolve(file);
    const existing = Array.from(
      document.querySelectorAll<HTMLLinkElement>(
        `link[data-plugin-id="${CSS.escape(manifest.id)}"]`,
      ),
    ).find((link) => link.dataset.cssFile === file);
    if (existing) {
      existing.href = href;
      continue;
    }
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = href;
    link.dataset.pluginId = manifest.id;
    link.dataset.cssFile = file;
    document.head.appendChild(link);
  }
}

function removePluginCss(pluginId: string) {
  document
    .querySelectorAll(`link[data-plugin-id="${CSS.escape(pluginId)}"]`)
    .forEach((link) => link.remove());
}

export function PluginRegistryProvider({
  children,
  backendUrl,
}: PluginRegistryProviderProps) {
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const resolvedBackendUrl = useMemo(() => {
    if (typeof window === 'undefined') return backendUrl;
    try {
      return new URL(backendUrl).toString();
    } catch {
      return new URL(backendUrl, window.location.origin).toString();
    }
  }, [backendUrl]);

  const [registry] = useState(() =>
    createPluginRegistry({
      fetchActive: async () => {
        try {
          const { data, error } = await apiClient.GET('/plugins/active');
          if (!error && Array.isArray(data)) return data;
          console.warn('Failed to fetch active plugins:', error ?? data);
        } catch (error) {
          console.warn('Failed to fetch active plugins:', error);
        }
        return null;
      },
      importRemote: async (manifest, fresh) => {
        const buster = fresh ? cacheBuster() : null;
        const resolve = (file: string) => {
          const url = new URL(file, resolvedBackendUrl);
          if (buster) url.searchParams.set('_ts', buster);
          return url.toString();
        };
        injectPluginCss(manifest, resolve);
        // A translation-only plugin has no entry point.
        return manifest.entry
          ? ((await import(
              /* @vite-ignore */ resolve(manifest.entry)
            )) as PluginModule)
          : ({ manifest } as PluginModule);
      },
      removeRemote: removePluginCss,
      // `onInit` may fail on a transient error too, so every failure the
      // registry does not rule out is worth another try.
      retry: (load, shouldRetry) =>
        retryLoad(load, { attempts: PLUGIN_LOAD_ATTEMPTS, shouldRetry }),
      refreshI18n: async () => {
        await queryClient.refetchQueries({
          queryKey: ['i18n'],
          type: 'active',
        });
      },
    }),
  );

  const loads = useSyncExternalStore(
    registry.subscribe,
    registry.getLoads,
    registry.getLoads,
  );
  const view = useMemo(() => registryView(loads), [loads]);

  // Until the server's list has been fetched once, pages show a loading
  // state rather than an empty one.
  const [listed, setListed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const syncUntilLoaded = async (attempt: number) => {
      const result = await registry.sync();
      if (cancelled) return;
      if (result !== 'unreachable') setListed(true);
      if (result !== 'ok') {
        timer = setTimeout(
          () => void syncUntilLoaded(attempt + 1),
          backoffDelay(attempt, SYNC_RETRY),
        );
      }
    };
    void syncUntilLoaded(0);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // The plugin set is fixed for the provider's lifetime.
  }, [registry]);

  const loadPlugin = useCallback(
    (manifest: ActivePluginManifest, module: PluginModule) =>
      registry.load(manifest, 'local', async () => module),
    [registry],
  );

  const loadAllPlugins = useCallback(async () => {
    await registry.sync();
  }, [registry]);

  const { plugins, components, routes, errors, loading } = view;

  const getSlots = useCallback(
    (slotName: string): SlotConfig[] => {
      const slots: SlotConfig[] = [];
      plugins.forEach((plugin, pluginName) => {
        for (const slot of plugin.slots ?? []) {
          if (slot.name === slotName) {
            slots.push({ ...slot, _pluginName: pluginName });
          }
        }
      });
      // Higher priority first.
      return slots.sort((a, b) => (b.priority || 0) - (a.priority || 0));
    },
    [plugins],
  );

  const value = useMemo(
    () => ({
      plugins,
      components,
      routes,
      isLoading: !listed || loading,
      loadPlugin,
      loadAllPlugins,
      unloadPlugin: registry.unload,
      reloadPlugin: registry.reload,
      reloadAllPlugins: registry.reloadAll,
      getSlots,
      errors,
    }),
    [
      plugins,
      components,
      routes,
      listed,
      loading,
      loadPlugin,
      loadAllPlugins,
      registry,
      getSlots,
      errors,
    ],
  );

  return (
    <PluginRegistryContext value={value}>{children}</PluginRegistryContext>
  );
}
