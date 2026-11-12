import { parseApiError, useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { PluginDetail } from '@broccoli/web-sdk/plugin';
import {
  Label,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { ConfigForm } from './ConfigForm';
import type { ConfigScope, InheritedConfig } from './types';

type ConfigSchemaResponse = PluginDetail['config_schemas'][number];
type PluginDetailResponse = PluginDetail;

/** A config entry returned by resource-scoped config list endpoints.
 *  Mirrors the generated `PluginConfigResponse` schema shape. */
interface ConfigEntry {
  plugin_id: string;
  namespace: string;
  config: unknown;
  enabled?: boolean | null;
  position: number;
  updated_at?: string | null;
  json_schema?: unknown;
  description?: string | null;
}

/** The enabled toggle's "inherit from the parent scope" option. */
const INHERIT = 'unset';

export interface ResourceConfigDialogProps {
  scope: ConfigScope;
  resourceLabel: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

// Stable empty fallbacks. Disabled queries would otherwise produce a fresh
// `[]` default on every render, cascading new identities through the memos
// below and re-firing every effect that depends on them.
const EMPTY_PLUGIN_DETAILS: PluginDetailResponse[] = [];
const EMPTY_CONFIG_ENTRIES: ConfigEntry[] = [];
const EMPTY_PLUGIN_SCHEMAS: {
  pluginId: string;
  schemas: ConfigSchemaResponse[];
}[] = [];

function buildConfigCallbacks(
  apiClient: ReturnType<typeof useApiClient>,
  scope: ConfigScope,
  pluginId: string,
  namespace: string,
) {
  switch (scope.scope) {
    case 'plugin':
      return {
        getConfig: async () => {
          const { data, error } = await apiClient.GET(
            '/admin/plugins/{id}/config',
            {
              params: {
                path: { id: scope.pluginId },
              },
            },
          );
          if (error) throw error;
          const entry = data?.find(
            (item: ConfigEntry) => item.namespace === namespace,
          );
          return (entry?.config ?? {}) as Record<string, unknown>;
        },
        putConfig: async (config: Record<string, unknown>) => {
          const { error } = await apiClient.PUT(
            '/admin/plugins/{id}/config/{namespace}',
            {
              params: {
                path: { id: scope.pluginId, namespace },
              },
              body: { config },
            },
          );
          return { error };
        },
        deleteConfig: async () => {
          const { error } = await apiClient.DELETE(
            '/admin/plugins/{id}/config/{namespace}',
            {
              params: {
                path: { id: scope.pluginId, namespace },
              },
            },
          );
          return { error };
        },
      };
    case 'contest':
      return {
        getConfig: async () => {
          const { data, error } = await apiClient.GET(
            '/contests/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.contestId, plugin_id: pluginId, namespace },
              },
            },
          );
          if (error) throw error;
          return (data?.config ?? {}) as Record<string, unknown>;
        },
        putConfig: async (
          config: Record<string, unknown>,
          enabled?: boolean | null,
        ) => {
          const { error } = await apiClient.PUT(
            '/contests/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.contestId, plugin_id: pluginId, namespace },
              },
              body: { config, ...(enabled !== undefined ? { enabled } : {}) },
            },
          );
          return { error };
        },
        deleteConfig: async () => {
          const { error } = await apiClient.DELETE(
            '/contests/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.contestId, plugin_id: pluginId, namespace },
              },
            },
          );
          return { error };
        },
      };
    case 'problem':
      return {
        getConfig: async () => {
          const { data, error } = await apiClient.GET(
            '/problems/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.problemId, plugin_id: pluginId, namespace },
              },
            },
          );
          if (error) throw error;
          return (data?.config ?? {}) as Record<string, unknown>;
        },
        putConfig: async (
          config: Record<string, unknown>,
          enabled?: boolean | null,
        ) => {
          const { error } = await apiClient.PUT(
            '/problems/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.problemId, plugin_id: pluginId, namespace },
              },
              body: { config, ...(enabled !== undefined ? { enabled } : {}) },
            },
          );
          return { error };
        },
        deleteConfig: async () => {
          const { error } = await apiClient.DELETE(
            '/problems/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.problemId, plugin_id: pluginId, namespace },
              },
            },
          );
          return { error };
        },
      };
    case 'contest_problem':
      return {
        getConfig: async () => {
          const { data, error } = await apiClient.GET(
            '/contests/{id}/problems/{problem_id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: {
                  id: scope.contestId,
                  problem_id: scope.problemId,
                  plugin_id: pluginId,
                  namespace,
                },
              },
            },
          );
          if (error) throw error;
          return (data?.config ?? {}) as Record<string, unknown>;
        },
        putConfig: async (config: Record<string, unknown>) => {
          const { error } = await apiClient.PUT(
            '/contests/{id}/problems/{problem_id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: {
                  id: scope.contestId,
                  problem_id: scope.problemId,
                  plugin_id: pluginId,
                  namespace,
                },
              },
              body: { config },
            },
          );
          return { error };
        },
        deleteConfig: async () => {
          const { error } = await apiClient.DELETE(
            '/contests/{id}/problems/{problem_id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: {
                  id: scope.contestId,
                  problem_id: scope.problemId,
                  plugin_id: pluginId,
                  namespace,
                },
              },
            },
          );
          return { error };
        },
      };
  }
}

/** Fetch the config list for a resource-scoped config (problem/contest/contest_problem).
 *  Returns config entries with embedded json_schema from the self-describing endpoint. */
function useResourceConfigList(
  apiClient: ReturnType<typeof useApiClient>,
  scope: ConfigScope,
  open: boolean,
) {
  return useQuery<ConfigEntry[]>({
    queryKey: configListQueryKey(scope),
    queryFn: async () => {
      let result: { data?: unknown; error?: unknown };

      switch (scope.scope) {
        case 'problem':
          result = await apiClient.GET('/problems/{id}/config', {
            params: { path: { id: scope.problemId } },
          });
          break;
        case 'contest':
          result = await apiClient.GET('/contests/{id}/config', {
            params: { path: { id: scope.contestId } },
          });
          break;
        case 'contest_problem':
          result = await apiClient.GET(
            '/contests/{id}/problems/{problem_id}/config',
            {
              params: {
                path: {
                  id: scope.contestId,
                  problem_id: scope.problemId,
                },
              },
            },
          );
          break;
        default:
          return [];
      }

      if (result.error) throw result.error;
      return (result.data ?? []) as ConfigEntry[]; // All three endpoints return PluginConfigResponse[]
    },
    enabled: open && scope.scope !== 'plugin',
  });
}

function configListQueryKey(scope: ConfigScope): string[] {
  switch (scope.scope) {
    case 'problem':
      return ['config-list', 'problem', String(scope.problemId)];
    case 'contest':
      return ['config-list', 'contest', String(scope.contestId)];
    case 'contest_problem':
      return [
        'config-list',
        'contest_problem',
        String(scope.contestId),
        String(scope.problemId),
      ];
    case 'plugin':
      return ['config-list', 'plugin', scope.pluginId];
  }
}

/** Convert config list entries into the pluginsWithSchemas structure the dialog expects. */
function configEntriesToPluginSchemas(
  entries: ConfigEntry[],
): { pluginId: string; schemas: ConfigSchemaResponse[] }[] {
  const byPlugin = new Map<
    string,
    { pluginId: string; schemas: ConfigSchemaResponse[] }
  >();

  for (const entry of entries) {
    if (!entry.json_schema) continue;

    if (!byPlugin.has(entry.plugin_id)) {
      byPlugin.set(entry.plugin_id, {
        pluginId: entry.plugin_id,
        schemas: [],
      });
    }

    byPlugin.get(entry.plugin_id)!.schemas.push({
      namespace: entry.namespace,
      description: entry.description ?? undefined,
      scopes: [], // Not needed since we already know the scope matches
      json_schema: entry.json_schema as Record<string, unknown>,
    });
  }

  return Array.from(byPlugin.values());
}

/** For plugin scope, use GET /admin/plugins (admin-only). */
function usePluginScopeSchemas(
  apiClient: ReturnType<typeof useApiClient>,
  scope: ConfigScope,
  open: boolean,
) {
  const { data: plugins = EMPTY_PLUGIN_DETAILS } = useQuery({
    queryKey: ['admin-plugins'],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/admin/plugins');
      if (error) throw error;
      return data;
    },
    enabled: open && scope.scope === 'plugin',
  });

  // Depend on the primitive plugin id, not the scope object (callers pass
  // inline scope literals, so the object identity changes on every render).
  const scopePluginId = scope.scope === 'plugin' ? scope.pluginId : null;

  return useMemo(() => {
    if (scopePluginId === null) return EMPTY_PLUGIN_SCHEMAS;
    return plugins
      .filter((p: PluginDetailResponse) => p.id === scopePluginId)
      .map((p: PluginDetailResponse) => ({
        pluginId: p.id,
        schemas: p.config_schemas.filter((s: ConfigSchemaResponse) =>
          s.scopes.includes('plugin'),
        ),
      }))
      .filter((e: { schemas: ConfigSchemaResponse[] }) => e.schemas.length > 0);
  }, [plugins, scopePluginId]);
}

export function ResourceConfigDialog({
  scope,
  resourceLabel,
  open,
  onOpenChange,
}: ResourceConfigDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();

  // For plugin scope, use admin endpoint
  const pluginSchemas = usePluginScopeSchemas(apiClient, scope, open);

  // For resource scopes, use self-describing config list endpoint
  const { data: configEntries = EMPTY_CONFIG_ENTRIES } = useResourceConfigList(
    apiClient,
    scope,
    open,
  );

  const scopeKind = scope.scope;
  const pluginsWithSchemas = useMemo(() => {
    if (scopeKind === 'plugin') return pluginSchemas;
    return configEntriesToPluginSchemas(configEntries);
  }, [scopeKind, pluginSchemas, configEntries]);

  const [activePlugin, setActivePlugin] = useState('');
  const [activeNamespace, setActiveNamespace] = useState('');

  // Initialize/repair the selection without ever clobbering a valid one:
  // only fall back to the first plugin when the current selection is empty
  // or no longer present (e.g. the dialog was reused for another resource).
  useEffect(() => {
    if (!open || pluginsWithSchemas.length === 0) return;
    const current = pluginsWithSchemas.find((e) => e.pluginId === activePlugin);
    if (!current) {
      setActivePlugin(pluginsWithSchemas[0].pluginId);
      setActiveNamespace(pluginsWithSchemas[0].schemas[0]?.namespace ?? '');
      return;
    }
    if (!current.schemas.some((s) => s.namespace === activeNamespace)) {
      setActiveNamespace(current.schemas[0]?.namespace ?? '');
    }
  }, [open, pluginsWithSchemas, activePlugin, activeNamespace]);

  const invalidateKeys = [configListQueryKey(scope)];
  if (scope.scope === 'plugin') invalidateKeys.push(['admin-plugins']);
  if (scope.scope === 'contest' || scope.scope === 'contest_problem')
    invalidateKeys.push(['admin-contests']);
  if (scope.scope === 'problem' || scope.scope === 'contest_problem')
    invalidateKeys.push(['admin-problems']);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        size="3xl"
        className="flex flex-col overflow-hidden p-0"
      >
        <SheetHeader className="shrink-0 border-b px-6 py-4">
          <SheetTitle>{t('config.title', { name: resourceLabel })}</SheetTitle>
          <SheetDescription>{t('config.description')}</SheetDescription>
        </SheetHeader>

        <div className="flex-1 overflow-y-auto px-6 py-5">
          {pluginsWithSchemas.length === 0 ? (
            <div className="py-12 text-center text-muted-foreground text-sm">
              {t('config.noSchemas')}
            </div>
          ) : pluginsWithSchemas.length === 1 ? (
            <SinglePluginContent
              pluginId={pluginsWithSchemas[0].pluginId}
              schemas={pluginsWithSchemas[0].schemas}
              apiClient={apiClient}
              scope={scope}
              open={open}
              invalidateKeys={invalidateKeys}
            />
          ) : (
            <Tabs
              value={activePlugin}
              onValueChange={(v) => {
                setActivePlugin(v);
                const entry = pluginsWithSchemas.find((e) => e.pluginId === v);
                if (entry)
                  setActiveNamespace(entry.schemas[0]?.namespace ?? '');
              }}
            >
              <TabsList>
                {pluginsWithSchemas.map((entry) => (
                  <TabsTrigger key={entry.pluginId} value={entry.pluginId}>
                    {entry.pluginId}
                  </TabsTrigger>
                ))}
              </TabsList>
              {pluginsWithSchemas.map((entry) => (
                <TabsContent key={entry.pluginId} value={entry.pluginId}>
                  <SinglePluginContent
                    pluginId={entry.pluginId}
                    schemas={entry.schemas}
                    apiClient={apiClient}
                    scope={scope}
                    open={open && activePlugin === entry.pluginId}
                    invalidateKeys={invalidateKeys}
                    activeNamespace={activeNamespace}
                    onNamespaceChange={setActiveNamespace}
                  />
                </TabsContent>
              ))}
            </Tabs>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}

function SinglePluginContent({
  pluginId,
  schemas,
  apiClient,
  scope,
  open,
  invalidateKeys,
  activeNamespace: controlledNamespace,
  onNamespaceChange,
}: {
  pluginId: string;
  schemas: ConfigSchemaResponse[];
  apiClient: ReturnType<typeof useApiClient>;
  scope: ConfigScope;
  open: boolean;
  invalidateKeys: string[][];
  activeNamespace?: string;
  onNamespaceChange?: (ns: string) => void;
}) {
  const { t } = useTranslation();
  const [localNamespace, setLocalNamespace] = useState(
    schemas[0]?.namespace ?? '',
  );
  const activeNs = controlledNamespace ?? localNamespace;
  const setActiveNs = onNamespaceChange ?? setLocalNamespace;

  const showEnabledToggle =
    scope.scope === 'contest' || scope.scope === 'problem';
  // 'unset' (no config row), true, or false.
  const [enabled, setEnabled] = useState<'unset' | boolean>('unset');
  const enabledRef = useRef<'unset' | boolean>('unset');
  enabledRef.current = enabled;

  useEffect(() => {
    if (!open || !showEnabledToggle || schemas.length === 0) return;
    let cancelled = false;

    const fetchRowEnabled = async (
      namespace: string,
    ): Promise<boolean | null> => {
      try {
        if (scope.scope === 'contest') {
          const { data } = await apiClient.GET(
            '/contests/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.contestId, plugin_id: pluginId, namespace },
              },
            },
          );
          return data?.enabled ?? null;
        }
        if (scope.scope === 'problem') {
          const { data } = await apiClient.GET(
            '/problems/{id}/config/{plugin_id}/{namespace}',
            {
              params: {
                path: { id: scope.problemId, plugin_id: pluginId, namespace },
              },
            },
          );
          return data?.enabled ?? null;
        }
        return null;
      } catch {
        return null;
      }
    };

    // Enablement is stored per (plugin, namespace) config row; the server
    // merges same-scope rows with true-wins semantics (hooks.rs). Load ALL
    // rows and apply the same merge so the toggle reflects what the server
    // will actually do: any true wins, else any false, else unset.
    Promise.all(schemas.map((s) => fetchRowEnabled(s.namespace))).then(
      (rows) => {
        if (cancelled) return;
        setEnabled(
          rows.includes(true) ? true : rows.includes(false) ? false : 'unset',
        );
      },
    );

    return () => {
      cancelled = true;
    };
  }, [open, showEnabledToggle, schemas, apiClient, scope, pluginId]);

  // Same guard as the plugin-level selection: never clobber a still-valid
  // namespace selection when the schema list refreshes (e.g. after a save
  // invalidates the config list query).
  useEffect(() => {
    if (open && schemas.length > 0 && !controlledNamespace) {
      setLocalNamespace((ns) =>
        schemas.some((s) => s.namespace === ns) ? ns : schemas[0].namespace,
      );
    }
  }, [open, schemas, controlledNamespace]);

  const [inheritedByNamespace, setInheritedByNamespace] = useState<
    Record<string, InheritedConfig>
  >({});

  useEffect(() => {
    if (!open || scope.scope !== 'contest_problem' || schemas.length === 0)
      return;

    let cancelled = false;
    const s = scope as { contestId: number; problemId: number };

    Promise.all(
      schemas.map(async (schema) => {
        const [contestRes, problemRes] = await Promise.all([
          apiClient
            .GET('/contests/{id}/config/{plugin_id}/{namespace}', {
              params: {
                path: {
                  id: s.contestId,
                  plugin_id: pluginId,
                  namespace: schema.namespace,
                },
              },
            })
            .catch(() => ({ data: undefined })),
          apiClient
            .GET('/problems/{id}/config/{plugin_id}/{namespace}', {
              params: {
                path: {
                  id: s.problemId,
                  plugin_id: pluginId,
                  namespace: schema.namespace,
                },
              },
            })
            .catch(() => ({ data: undefined })),
        ]);

        const inherited: InheritedConfig = {
          contest: contestRes.data
            ? {
                values:
                  (contestRes.data.config as Record<string, unknown>) ?? null,
                enabled:
                  (contestRes.data as { enabled?: boolean | null }).enabled ===
                  true,
              }
            : undefined,
          problem: problemRes.data
            ? {
                values:
                  (problemRes.data.config as Record<string, unknown>) ?? null,
                enabled:
                  (problemRes.data as { enabled?: boolean | null }).enabled ===
                  true,
              }
            : undefined,
        };

        return [schema.namespace, inherited] as const;
      }),
    ).then((entries) => {
      if (cancelled) return;
      setInheritedByNamespace(Object.fromEntries(entries));
    });

    return () => {
      cancelled = true;
    };
  }, [open, scope, pluginId, schemas, apiClient]);

  const rawCallbacks = useMemo(
    () =>
      Object.fromEntries(
        schemas.map((s) => [
          s.namespace,
          buildConfigCallbacks(apiClient, scope, pluginId, s.namespace),
        ]),
      ),
    [apiClient, scope, pluginId, schemas],
  );

  // The enabled toggle is form-local state - persisted via the form's Save
  // button, not auto-saved. This avoids a race condition where the toggle's
  // read-modify-write overlaps with a concurrent form save on slow networks.
  const handleEnabledChange = useCallback((newEnabled: 'unset' | boolean) => {
    setEnabled(newEnabled);
    enabledRef.current = newEnabled;
  }, []);

  // The UI presents enablement as a single per-plugin choice, but it is
  // persisted on every (plugin, namespace) row and the server merges rows
  // with true-wins semantics. Writing it only to the saved namespace would
  // let a stale `true` on another row silently override a disable, so after
  // a save we rewrite every OTHER existing row with the same enabled value,
  // preserving that row's config and position. Rows that do not exist stay
  // absent: they contribute nothing to the server-side merge.
  const propagateEnabled = useCallback(
    async (
      savedNamespace: string,
      enabled: boolean | undefined,
    ): Promise<{ error?: unknown }> => {
      for (const schema of schemas) {
        const namespace = schema.namespace;
        if (namespace === savedNamespace) continue;

        try {
          if (scope.scope === 'contest') {
            const path = {
              id: scope.contestId,
              plugin_id: pluginId,
              namespace,
            };
            const { data: row, error: getError } = await apiClient.GET(
              '/contests/{id}/config/{plugin_id}/{namespace}',
              { params: { path } },
            );
            if (getError) {
              // No row for this namespace is fine; anything else must fail
              // the save so the admin knows enablement is inconsistent.
              if (parseApiError(getError)?.code === 'NOT_FOUND') continue;
              return { error: getError };
            }
            if (!row || (row.enabled ?? undefined) === enabled) continue;
            const { error } = await apiClient.PUT(
              '/contests/{id}/config/{plugin_id}/{namespace}',
              {
                params: { path },
                body: {
                  config: row.config ?? {},
                  position: row.position,
                  ...(enabled !== undefined ? { enabled } : {}),
                },
              },
            );
            if (error) return { error };
          } else if (scope.scope === 'problem') {
            const path = {
              id: scope.problemId,
              plugin_id: pluginId,
              namespace,
            };
            const { data: row, error: getError } = await apiClient.GET(
              '/problems/{id}/config/{plugin_id}/{namespace}',
              { params: { path } },
            );
            if (getError) {
              if (parseApiError(getError)?.code === 'NOT_FOUND') continue;
              return { error: getError };
            }
            if (!row || (row.enabled ?? undefined) === enabled) continue;
            const { error } = await apiClient.PUT(
              '/problems/{id}/config/{plugin_id}/{namespace}',
              {
                params: { path },
                body: {
                  config: row.config ?? {},
                  position: row.position,
                  ...(enabled !== undefined ? { enabled } : {}),
                },
              },
            );
            if (error) return { error };
          }
        } catch (error) {
          return { error };
        }
      }
      return {};
    },
    [schemas, scope, apiClient, pluginId],
  );

  const callbacksByNamespace = useMemo(() => {
    if (!showEnabledToggle) return rawCallbacks;
    return Object.fromEntries(
      Object.entries(rawCallbacks).map(([ns, cbs]) => [
        ns,
        {
          ...cbs,
          putConfig: async (config: Record<string, unknown>) => {
            const enabledValue = enabledRef.current;
            const enabled = enabledValue === 'unset' ? undefined : enabledValue;
            const result = await (
              cbs.putConfig as (
                c: Record<string, unknown>,
                e?: boolean,
              ) => Promise<{ error?: unknown }>
            )(config, enabled);
            if (result.error) return result;
            return propagateEnabled(ns, enabled);
          },
        },
      ]),
    );
  }, [rawCallbacks, showEnabledToggle, propagateEnabled]);

  const enabledToggle = showEnabledToggle ? (
    <div className="rounded-lg border p-3 space-y-2">
      <Label className="text-sm font-medium">
        {t('plugins.config.pluginStatus')}
      </Label>
      <div className="grid grid-cols-3 rounded-lg border border-border overflow-hidden bg-muted text-center divide-x divide-border">
        {(
          [
            { key: INHERIT, label: t('plugins.config.inherit') },
            { key: true, label: t('plugins.config.enabledLabel') },
            { key: false, label: t('plugins.config.disabledLabel') },
          ] as const
        ).map((opt) => {
          const isCurrent = enabled === opt.key;
          return (
            <button
              key={String(opt.key)}
              type="button"
              onClick={() => handleEnabledChange(opt.key as 'unset' | boolean)}
              className={`py-2 px-2 text-xs border-none transition-all duration-150 cursor-pointer ${
                isCurrent
                  ? 'bg-card font-semibold opacity-100'
                  : 'bg-transparent opacity-60 hover:opacity-80'
              }`}
            >
              {opt.label}
            </button>
          );
        })}
      </div>
      <p className="text-[11px] text-muted-foreground m-0">
        {enabled === 'unset'
          ? t('plugins.config.inheritHint')
          : enabled
            ? t('plugins.config.enabledHint')
            : t('plugins.config.disabledHint')}
      </p>
    </div>
  ) : null;

  if (schemas.length === 1) {
    return (
      <div className="space-y-4">
        {enabledToggle}
        <ConfigForm
          schema={schemas[0]}
          open={open}
          pluginId={pluginId}
          scope={scope}
          inherited={inheritedByNamespace[schemas[0].namespace]}
          {...callbacksByNamespace[schemas[0].namespace]}
          invalidateQueryKeys={invalidateKeys}
        />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {enabledToggle}
      <Tabs value={activeNs} onValueChange={setActiveNs}>
        <TabsList>
          {schemas.map((s) => (
            <TabsTrigger key={s.namespace} value={s.namespace}>
              {s.namespace}
            </TabsTrigger>
          ))}
        </TabsList>
        {schemas.map((s) => (
          <TabsContent key={s.namespace} value={s.namespace}>
            <ConfigForm
              schema={s}
              open={open && activeNs === s.namespace}
              pluginId={pluginId}
              scope={scope}
              inherited={inheritedByNamespace[s.namespace]}
              {...callbacksByNamespace[s.namespace]}
              invalidateQueryKeys={invalidateKeys}
            />
          </TabsContent>
        ))}
      </Tabs>
    </div>
  );
}
