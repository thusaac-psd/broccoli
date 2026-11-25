import {
  pluginKeys,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import type { PluginDetail } from '@broccoli/web-sdk/plugin';
import { Slot } from '@broccoli/web-sdk/slot';
import { Button, SheetFooter } from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { RotateCcw, Trash2 } from 'lucide-react';
import { useCallback, useMemo, useState } from 'react';

import { RecordLoader } from '@/components/RecordLoader';

import { SchemaFields } from './SchemaFields';
import type {
  ConfigScope,
  FieldProblem,
  InheritedConfig,
  JsonSchema,
} from './types';
import { useFieldName } from './use-field-name';
import { useFieldProblemText } from './use-field-problem-text';
import {
  deepMerge,
  extractDefaults,
  forWrite,
  hasOwnDescendantValue,
  hasOwnValueAtPath,
  markSaved,
  serverFieldErrors,
  setAtPath,
  unmarked,
  validateAll,
} from './utils';

type ConfigSchemaResponse = PluginDetail['config_schemas'][number];

export interface ConfigFormProps {
  schema: ConfigSchemaResponse;
  open: boolean;
  pluginId?: string;
  scope?: ConfigScope;
  inherited?: InheritedConfig;
  getConfig: () => Promise<Record<string, unknown>>;
  /** Writes `config`, answering the stored config as the server shows it
   * (each set secret its ref) or the error. */
  putConfig: (
    config: Record<string, unknown>,
  ) => Promise<{ error?: unknown; config?: Record<string, unknown> }>;
  deleteConfig: () => Promise<{ error?: unknown }>;
  invalidateQueryKeys?: string[][];
}

/** A user's edit of one field: the value now at `path`. */
interface FieldEdit {
  path: string[];
  value: unknown;
}

export function ConfigForm(props: ConfigFormProps) {
  const { schema, open, pluginId, scope, getConfig } = props;
  if (!open) return null;
  return (
    <RecordLoader
      key={`${pluginId ?? ''}:${schema.namespace}:${JSON.stringify(scope)}`}
      queryKey={['config-form', pluginId, schema.namespace, scope]}
      queryFn={() =>
        getConfig().catch((err: unknown) => {
          // Nothing stored yet: the form starts from the defaults.
          if ((err as { code?: unknown } | null)?.code === 'NOT_FOUND') {
            return {};
          }
          throw err;
        })
      }
    >
      {(stored) => <ConfigFormFields {...props} initialStored={stored} />}
    </RecordLoader>
  );
}

function ConfigFormFields({
  schema,
  pluginId,
  scope,
  inherited,
  initialStored,
  putConfig,
  deleteConfig,
  invalidateQueryKeys,
}: ConfigFormProps & { initialStored: Record<string, unknown> }) {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const problemText = useFieldProblemText();
  const pluginText = usePluginText();
  const jsonSchema = schema.json_schema as JsonSchema;
  const schemaDefaults = useMemo(
    () => extractDefaults(jsonSchema),
    [jsonSchema],
  );

  // What the scope shows where it stores nothing: what it inherits, as the
  // server resolved it, else (the plugin-wide scope) the schema's defaults.
  const defaults = useMemo(
    () =>
      inherited
        ? {
            ...schemaDefaults,
            ...((inherited.config as Record<string, unknown> | null) ?? {}),
          }
        : schemaDefaults,
    [schemaDefaults, inherited],
  );

  // Each item of a keyed array is marked with the key it was read under, so
  // the form tells a saved item from one added (or renamed) since.
  const [storedValues, setStoredValues] = useState(
    () => markSaved(initialStored, jsonSchema) as Record<string, unknown>,
  );
  // The form shows the stored config over the defaults (or the defaults
  // alone after "Defaults"), with the user's edits on top. Derived rather
  // than copied, so defaults that arrive later (inherited values) show up in
  // every field the user has not set.
  const [fromDefaults, setFromDefaults] = useState(false);
  const [edits, setEdits] = useState<ReadonlyMap<string, FieldEdit>>(
    () => new Map(),
  );
  const values = useMemo(() => {
    let result = fromDefaults ? defaults : deepMerge(defaults, storedValues);
    for (const { path, value } of edits.values()) {
      result = setAtPath(result, path, value);
    }
    return result;
  }, [defaults, storedValues, fromDefaults, edits]);

  const [errors, setErrors] = useState<Record<string, FieldProblem>>({});
  const [loading, setLoading] = useState(false);
  const [message, setMessage] = useState<{
    type: 'success' | 'error';
    text: string;
    /** The fields a refusal names, by path. A field a plugin's slot
     * component renders shows no inline error, so they are listed here too. */
    fields?: [string, FieldProblem][];
  } | null>(null);

  const isUsingDefaultsOnly = useMemo(
    () => !hasOwnDescendantValue(storedValues, []),
    [storedValues],
  );
  const isExplicitValue = useCallback(
    (path: string[]) => hasOwnValueAtPath(storedValues, path),
    [storedValues],
  );
  const hasExplicitDescendant = useCallback(
    (path: string[]) => hasOwnDescendantValue(storedValues, path),
    [storedValues],
  );

  const isDirty = useCallback(
    (path: string[]) => edits.has(path.join('.')),
    [edits],
  );

  const updateValue = useCallback((path: string[], value: unknown) => {
    if (path.length === 0) return;
    const dotPath = path.join('.');
    setEdits((prev) => {
      const next = new Map(prev);
      // Re-inserted last, so a later edit of a parent applies after it.
      next.delete(dotPath);
      next.set(dotPath, { path, value });
      return next;
    });

    setErrors((prev) => {
      if (!prev[dotPath]) return prev;
      const next = { ...prev };
      delete next[dotPath];
      return next;
    });
  }, []);

  /** A refused field as the form names it: its titles and its items. */
  const nameField = useFieldName();
  function fieldName(path: string): string {
    return nameField(
      { pluginId, namespace: schema.namespace, schema: jsonSchema },
      path,
      values,
    );
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();

    const validationErrors = validateAll(values, jsonSchema);
    if (Object.keys(validationErrors).length > 0) {
      setErrors(validationErrors);
      return;
    }

    setLoading(true);
    setMessage(null);

    const dirtyTopKeys = new Set<string>();
    for (const { path } of edits.values()) dirtyTopKeys.add(path[0]);
    if (fromDefaults) {
      for (const key of Object.keys(storedValues)) dirtyTopKeys.add(key);
    }
    const payload: Record<string, unknown> = {};
    for (const key of Object.keys(values)) {
      if (Object.hasOwn(storedValues, key) || dirtyTopKeys.has(key)) {
        payload[key] = values[key];
      }
    }

    const written = forWrite(payload, jsonSchema) as Record<string, unknown>;
    const { error, config: saved } = await putConfig(written);

    setLoading(false);
    if (error) {
      const fieldErrors = serverFieldErrors(error);
      setErrors(fieldErrors);
      setMessage({
        type: 'error',
        text: t('plugins.config.saveError'),
        fields: Object.entries(fieldErrors),
      });
    } else {
      // The server's answer: each secret it stores shows the ref it now
      // holds, which the next save sends back to keep it.
      setStoredValues(
        markSaved(saved ?? {}, jsonSchema) as Record<string, unknown>,
      );
      setFromDefaults(false);
      setEdits(new Map());
      setMessage({ type: 'success', text: t('plugins.config.saveSuccess') });
      if (invalidateQueryKeys) {
        for (const key of invalidateQueryKeys) {
          queryClient.invalidateQueries({ queryKey: key });
        }
      }
    }
  }

  function handleReset() {
    setFromDefaults(true);
    setEdits(new Map());
    setErrors({});
    setMessage(null);
  }

  async function handleDelete() {
    setLoading(true);
    setMessage(null);

    const { error } = await deleteConfig();

    setLoading(false);
    if (error) {
      setMessage({ type: 'error', text: t('plugins.config.deleteError') });
    } else {
      setStoredValues({});
      setFromDefaults(false);
      setEdits(new Map());
      setErrors({});
      setMessage({
        type: 'success',
        text: t('plugins.config.deleteSuccess'),
      });
      if (invalidateQueryKeys) {
        for (const key of invalidateQueryKeys) {
          queryClient.invalidateQueries({ queryKey: key });
        }
      }
    }
  }

  return (
    <form onSubmit={handleSubmit} className="space-y-5">
      {schema.description && (
        <p className="text-sm text-muted-foreground leading-relaxed">
          {pluginId
            ? pluginText(
                pluginKeys.configDescription(pluginId, schema.namespace),
                schema.description,
              )
            : schema.description}
        </p>
      )}

      {pluginId && (
        <Slot
          name={`config.form.${pluginId}.${schema.namespace}`}
          as="div"
          slotProps={{
            scope,
            pluginId,
            namespace: schema.namespace,
            values: unmarked(values),
            storedValues: unmarked(storedValues),
            schema: jsonSchema,
            isUsingDefaultsOnly,
            inherited,
            hasExplicitValue: (path: string | string[]) =>
              isExplicitValue(Array.isArray(path) ? path : [path]),
          }}
        />
      )}

      {isUsingDefaultsOnly && (
        <div className="rounded-lg border border-dashed bg-muted/40 px-4 py-3 text-sm text-muted-foreground">
          {t('plugins.config.unsetNotice')}
        </div>
      )}

      <div className="space-y-5">
        <SchemaFields
          schema={jsonSchema}
          values={values}
          rootValues={values}
          path={[]}
          updateValue={updateValue}
          errors={errors}
          pluginId={pluginId}
          namespace={schema.namespace}
          scope={scope}
          isExplicitValue={isExplicitValue}
          hasExplicitDescendant={hasExplicitDescendant}
          isDirty={isDirty}
          inherited={inherited}
        />
      </div>

      {message && (
        <div
          className={`rounded-lg px-4 py-3 text-sm ${
            message.type === 'success'
              ? 'bg-green-500/10 text-green-600 dark:text-green-400 border border-green-500/20'
              : 'bg-destructive/10 text-destructive border border-destructive/20'
          }`}
        >
          {message.text}
          {message.fields && message.fields.length > 0 && (
            <ul className="mt-1 list-disc pl-5">
              {message.fields.map(([path, problem]) => (
                <li key={path}>
                  {path ? (
                    <span className="font-medium">{fieldName(path)}</span>
                  ) : null}
                  {path ? ': ' : null}
                  {problemText(problem)}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <SheetFooter className="gap-2 sm:gap-0 pt-2">
        <div className="flex gap-2 mr-auto">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={handleReset}
            disabled={loading}
          >
            <RotateCcw className="h-3.5 w-3.5 mr-1.5" />
            {t('plugins.config.defaults')}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={handleDelete}
            disabled={loading}
            className="text-destructive hover:text-destructive"
            title={t('plugins.config.deleteHint')}
          >
            <Trash2 className="h-3.5 w-3.5 mr-1.5" />
            {t('plugins.config.delete')}
          </Button>
        </div>
        <Button type="submit" disabled={loading}>
          {loading ? t('plugins.config.saving') : t('plugins.config.save')}
        </Button>
      </SheetFooter>
    </form>
  );
}
