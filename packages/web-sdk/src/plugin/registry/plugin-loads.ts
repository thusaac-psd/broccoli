import type { ElementType } from 'react';

import type {
  ActivePluginManifest,
  ComponentBundle,
  PluginModule,
  RouteConfig,
} from '@/plugin/types';

// Each plugin the registry knows of is in exactly one of these states; one
// it does not know of is absent from the map.
//
// - `loading`: its module is being fetched and initialized. A second load
//   of the same plugin joins this one instead of starting another.
// - `loaded`: initialized; its components and routes are registered.
// - `failed`: every attempt failed. It stays so until it is reloaded.
//
// Components, routes and errors are derived from the map, never kept beside
// it, so a plugin is registered exactly while it is `loaded`. A load that
// settles after its plugin was unloaded (or reloaded) finds another entry,
// or none, in its place, and only cleans up after itself.

/** Where a plugin came from: the server's active list, or the host app. */
export type PluginSource = 'remote' | 'local';

export type PluginLoad = {
  manifest: ActivePluginManifest;
  source: PluginSource;
} & (
  | { status: 'loading'; promise: Promise<void> }
  | { status: 'loaded'; module: PluginModule }
  | { status: 'failed'; error: Error }
);

export type PluginLoads = ReadonlyMap<string, PluginLoad>;

export interface PluginRegistryDeps {
  /** The server's active plugins, or null when it could not be asked. */
  fetchActive: () => Promise<ActivePluginManifest[] | null>;
  /**
   * Fetch a remote plugin's module (and stylesheets). `fresh` bypasses
   * caches: set on retries and reloads.
   */
  importRemote: (
    manifest: ActivePluginManifest,
    fresh: boolean,
  ) => Promise<PluginModule>;
  /** Drop what `importRemote` added to the page (its stylesheets). */
  removeRemote: (pluginId: string) => void;
  /**
   * Run `load` until it succeeds, gives up, or fails in a way `shouldRetry`
   * rejects (see `retryLoad`).
   */
  retry: <T>(
    load: (attempt: number) => Promise<T>,
    shouldRetry: (error: unknown) => boolean,
  ) => Promise<T>;
  /** Refetch translations once the set of plugins has changed. */
  refreshI18n: () => Promise<void>;
}

/** Thrown by a load whose plugin was unloaded or reloaded meanwhile. */
const ABANDONED = new Error('The plugin load was abandoned.');

const toError = (error: unknown) =>
  error instanceof Error ? error : new Error(String(error));

async function destroy(manifest: ActivePluginManifest, module: PluginModule) {
  try {
    await module.onDestroy?.();
  } catch (error) {
    console.error(`Error unloading plugin '${manifest.id}':`, error);
  }
}

export function createPluginRegistry(deps: PluginRegistryDeps) {
  let loads: PluginLoads = new Map();
  const listeners = new Set<() => void>();

  const put = (id: string, load: PluginLoad | null) => {
    const next = new Map(loads);
    if (load) next.set(id, load);
    else next.delete(id);
    loads = next;
    listeners.forEach((listener) => listener());
  };

  /**
   * Load one plugin with `importModule`, unless it is loading or loaded
   * already. Failures (of the import or of `onInit`) are retried; once the
   * retries are spent the plugin is `failed`. The module is imported once: a
   * failed `onInit` runs again on the same module, since a second copy would
   * not share state with the chunks the first one loaded. `importModule` is
   * told to bypass caches only after an import failed.
   */
  const load = (
    manifest: ActivePluginManifest,
    source: PluginSource,
    importModule: (fresh: boolean) => Promise<PluginModule>,
  ): Promise<void> => {
    const existing = loads.get(manifest.id);
    if (existing?.status === 'loading') return existing.promise;
    if (existing?.status === 'loaded') return Promise.resolve();

    const isCurrent = () => {
      const entry = loads.get(manifest.id);
      return entry?.status === 'loading' && entry.promise === promise;
    };
    const promise: Promise<void> = (async () => {
      // Lets the entry below be in place before the first attempt starts.
      await Promise.resolve();
      let imported: PluginModule | null = null;
      let importFailed = false;
      try {
        const module = await deps.retry(
          async () => {
            // Unloaded while waiting to retry: no stylesheet, no onInit.
            if (!isCurrent()) throw ABANDONED;
            if (!imported) {
              try {
                imported = await importModule(importFailed);
              } catch (error) {
                importFailed = true;
                throw error;
              }
            }
            const candidate = imported;
            try {
              await candidate.onInit?.();
            } catch (error) {
              await destroy(manifest, candidate);
              throw error;
            }
            return candidate;
          },
          (error) => error !== ABANDONED,
        );
        if (isCurrent()) {
          put(manifest.id, { manifest, source, status: 'loaded', module });
        } else {
          await destroy(manifest, module);
        }
      } catch (error) {
        if (isCurrent()) {
          console.error(`Error loading plugin '${manifest.id}':`, error);
          if (source === 'remote') deps.removeRemote(manifest.id);
          put(manifest.id, {
            manifest,
            source,
            status: 'failed',
            error: toError(error),
          });
        }
      }
    })();
    put(manifest.id, { manifest, source, status: 'loading', promise });
    return promise;
  };

  const loadRemote = (manifest: ActivePluginManifest, fresh: boolean) =>
    load(manifest, 'remote', (retrying) =>
      deps.importRemote(manifest, fresh || retrying),
    );

  const unload = async (pluginId: string) => {
    const entry = loads.get(pluginId);
    if (!entry) return;
    put(pluginId, null);
    if (entry.source === 'remote') deps.removeRemote(pluginId);
    if (entry.status === 'loaded') await destroy(entry.manifest, entry.module);
  };

  /**
   * Bring the remote plugins in line with the server's active list: unload
   * those it no longer lists, load those not loaded yet. Failed plugins are
   * left for `reload`. Resolves to false when the list could not be fetched.
   */
  const sync = async (fresh = false): Promise<boolean> => {
    const active = await deps.fetchActive();
    if (!active) return false;
    const listed = new Set(active.map((manifest) => manifest.id));
    const removed = [...loads.values()].filter(
      (entry) => entry.source === 'remote' && !listed.has(entry.manifest.id),
    );
    await Promise.all(removed.map((entry) => unload(entry.manifest.id)));
    await Promise.all(
      active
        .filter((manifest) => loads.get(manifest.id)?.status !== 'failed')
        .map((manifest) => loadRemote(manifest, fresh)),
    );
    await deps.refreshI18n();
    return true;
  };

  /** Unload one remote plugin and load it afresh from the server. */
  const reload = async (pluginId: string) => {
    await unload(pluginId);
    const active = await deps.fetchActive();
    const manifest = active?.find((candidate) => candidate.id === pluginId);
    if (!manifest) {
      console.warn(`Plugin '${pluginId}' is not active on the server.`);
      return;
    }
    await loadRemote(manifest, true);
    await deps.refreshI18n();
  };

  /** Unload every remote plugin and load the server's list afresh. */
  const reloadAll = async () => {
    const remote = [...loads.values()].filter(
      (entry) => entry.source === 'remote',
    );
    await Promise.all(remote.map((entry) => unload(entry.manifest.id)));
    await sync(true);
  };

  return {
    getLoads: () => loads,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    load,
    unload,
    sync,
    reload,
    reloadAll,
  };
}

export type PluginRegistry = ReturnType<typeof createPluginRegistry>;

/** What the loaded plugins contribute, derived from the load states. */
export interface RegistryView {
  plugins: Map<string, ActivePluginManifest>;
  components: ComponentBundle;
  routes: RouteConfig[];
  errors: Map<string, Error>;
  loading: boolean;
}

export function registryView(loads: PluginLoads): RegistryView {
  const plugins = new Map<string, ActivePluginManifest>();
  const components: ComponentBundle = {};
  const owners = new Map<string, string>();
  const routes: RouteConfig[] = [];
  const errors = new Map<string, Error>();
  let loading = false;

  for (const [id, entry] of loads) {
    if (entry.status === 'loading') loading = true;
    if (entry.status === 'failed') errors.set(id, entry.error);
    if (entry.status !== 'loaded') continue;
    const { manifest, module } = entry;
    plugins.set(id, manifest);
    for (const [key, name] of Object.entries(manifest.components ?? {})) {
      const component = module[name];
      if (!component) {
        console.warn(
          `Component '${name}' of plugin '${id}' is not exported by its module; skipping '${key}'.`,
        );
        continue;
      }
      const owner = owners.get(key);
      if (owner && owner !== id) {
        console.warn(
          `Component key '${key}' of plugin '${id}' overrides the one from plugin '${owner}'.`,
        );
      }
      owners.set(key, id);
      components[key] = component as ElementType;
    }
    routes.push(...(manifest.routes ?? []));
  }
  return { plugins, components, routes, errors, loading };
}
