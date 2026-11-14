import {
  pluginKeys,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import {
  InheritedAnnotation,
  InheritedBadge,
  resolveInheritedValue,
  Slot,
} from '@broccoli/web-sdk/slot';
import { Button, Input, Label, Switch } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { Minus, Plus } from 'lucide-react';

import { BlobRefField } from './BlobRefField';
import { FieldError } from './FieldError';
import { NumericInput } from './NumericInput';
import { SchemaFields } from './SchemaFields';
import type {
  ConfigScope,
  FieldProblem,
  InheritedConfig,
  JsonSchemaProperty,
} from './types';
import { defaultForType } from './utils';

function DefaultBadge() {
  const { t } = useTranslation();
  return (
    <span className="inline-flex items-center rounded-full border border-dashed px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
      {t('plugins.config.defaultBadge')}
    </span>
  );
}

export function SchemaField({
  name,
  prop,
  value,
  rootValues,
  path,
  updateValue,
  errors,
  pluginId,
  namespace,
  scope,
  isExplicitValue,
  hasExplicitDescendant,
  isDirty,
  inherited,
}: Readonly<{
  name: string;
  prop: JsonSchemaProperty;
  value: unknown;
  rootValues: Record<string, unknown>;
  path: string[];
  updateValue: (path: string[], value: unknown) => void;
  errors: Record<string, FieldProblem>;
  pluginId?: string;
  namespace?: string;
  scope?: ConfigScope;
  isExplicitValue: (path: string[]) => boolean;
  hasExplicitDescendant: (path: string[]) => boolean;
  isDirty?: (path: string[]) => boolean;
  inherited?: InheritedConfig;
}>) {
  const { t } = useTranslation();
  const pluginText = usePluginText();
  const fieldId = `cfg-${path.join('-')}`;
  // A plugin can name its fields in its own translation pack.
  const label =
    pluginId && namespace
      ? pluginText(
          pluginKeys.configField(pluginId, namespace, path, 'title'),
          prop.title ?? name,
        )
      : (prop.title ?? name);
  const description =
    pluginId && namespace
      ? pluginText(
          pluginKeys.configField(pluginId, namespace, path, 'description'),
          prop.description,
        )
      : prop.description;
  const dotPath = path.join('.');
  const error = errors[dotPath];
  const isFieldExplicit = isExplicitValue(path);
  const hasFieldOverride = hasExplicitDescendant(path);
  const isFieldDirty = isDirty?.(path) ?? false;

  const inheritedResult = resolveInheritedValue(name, inherited);
  const showAsPlaceholder = !isFieldExplicit && !isFieldDirty;

  const badge = (() => {
    if (isFieldExplicit || isFieldDirty) return null;
    if (inheritedResult)
      return <InheritedBadge source={inheritedResult.source} />;
    return <DefaultBadge />;
  })();

  const annotation = (() => {
    if (!inheritedResult) return null;
    return (
      <InheritedAnnotation
        source={inheritedResult.source}
        value={String(inheritedResult.value)}
        isOverride={isFieldExplicit || isFieldDirty}
      />
    );
  })();

  const slotName =
    pluginId && namespace
      ? `config.field.${pluginId}.${namespace}.${path.join('.')}`
      : undefined;

  const defaultField = renderField();

  if (slotName) {
    return (
      <Slot
        name={slotName}
        className="grid"
        slotProps={{
          value,
          schema: prop,
          onChange: (v: unknown) => updateValue(path, v),
          formValues: rootValues,
          setFieldValue: (fieldPath: string[], fieldValue: unknown) =>
            updateValue(fieldPath, fieldValue),
          path,
          scope,
          isExplicitValue: isFieldExplicit,
          hasExplicitDescendant: hasFieldOverride,
          isDirty: isFieldDirty,
          inherited,
          showAsPlaceholder,
          inheritedValue: inheritedResult?.value,
          inheritedSource: inheritedResult?.source,
        }}
      >
        {defaultField}
      </Slot>
    );
  }

  return defaultField;

  function renderField() {
    // Blob ref -> file upload widget
    if (prop.type === 'object' && prop.format === 'blob-ref') {
      return (
        <BlobRefField
          label={label}
          description={description}
          value={value as { filename: string; hash: string } | undefined}
          onChange={(v) => updateValue(path, v)}
          isExplicit={isFieldExplicit}
        />
      );
    }

    // Object -> card-like grouped section
    if (prop.type === 'object' && prop.properties) {
      const objValue =
        value && typeof value === 'object'
          ? (value as Record<string, unknown>)
          : {};

      return (
        <div className="rounded-lg border bg-muted/30">
          <div className="px-4 py-3 border-b bg-muted/40 rounded-t-lg">
            <div className="flex items-center gap-2">
              <h4 className="text-sm font-medium">{label}</h4>
              {!hasFieldOverride && <DefaultBadge />}
            </div>
            {description && (
              <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">
                {description}
              </p>
            )}
          </div>
          <div className="p-4 pt-5">
            <SchemaFields
              schema={{ type: 'object' as const, properties: prop.properties }}
              values={objValue}
              rootValues={rootValues}
              path={path}
              updateValue={updateValue}
              errors={errors}
              pluginId={pluginId}
              namespace={namespace}
              scope={scope}
              isExplicitValue={isExplicitValue}
              hasExplicitDescendant={hasExplicitDescendant}
              isDirty={isDirty}
              inherited={inherited}
            />
          </div>
        </div>
      );
    }

    // Boolean -> horizontal settings-style card with switch on the right
    if (prop.type === 'boolean') {
      const inheritedBoolLabel =
        showAsPlaceholder && inheritedResult
          ? `(${String(inheritedResult.value)})`
          : undefined;

      return (
        <div className="flex items-center justify-between gap-4 rounded-lg border px-4 py-3">
          <div className="space-y-0.5">
            <div className="flex items-center gap-2">
              <Label
                htmlFor={fieldId}
                className="text-sm font-medium cursor-pointer"
              >
                {label}
              </Label>
              {badge}
              {inheritedBoolLabel && (
                <span className="text-[10px] text-muted-foreground/50 italic">
                  {inheritedBoolLabel}
                </span>
              )}
            </div>
            {description && (
              <p className="text-xs text-muted-foreground leading-relaxed">
                {description}
              </p>
            )}
            {annotation}
          </div>
          <Switch
            id={fieldId}
            checked={Boolean(value)}
            onCheckedChange={(v) => updateValue(path, v)}
          />
        </div>
      );
    }

    // String enum -> styled select
    if (prop.type === 'string' && prop.enum) {
      const selectPlaceholder =
        showAsPlaceholder && inheritedResult
          ? String(inheritedResult.value)
          : undefined;

      return (
        <div className="flex flex-col gap-1.5">
          <div className="space-y-1">
            <div className="flex items-center gap-2">
              <Label
                htmlFor={fieldId}
                className="text-xs font-medium text-muted-foreground uppercase tracking-wider"
              >
                {label}
              </Label>
              {badge}
            </div>
            {description && (
              <p className="text-xs text-muted-foreground">{description}</p>
            )}
          </div>
          <div>
            <select
              id={fieldId}
              value={
                showAsPlaceholder ? '' : typeof value === 'string' ? value : ''
              }
              onChange={(e) => updateValue(path, e.target.value)}
              className={cn(
                'flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-xs transition-colors focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring',
                showAsPlaceholder && 'text-muted-foreground',
              )}
            >
              {selectPlaceholder && (
                <option value="" disabled>
                  {selectPlaceholder}
                </option>
              )}
              {prop.enum.map((opt) => (
                <option key={String(opt)} value={String(opt)}>
                  {String(opt)}
                </option>
              ))}
            </select>
            <FieldError problem={error} />
            {annotation}
          </div>
        </div>
      );
    }

    // Number / integer -> custom NumericInput
    if (prop.type === 'number' || prop.type === 'integer') {
      const inheritedPlaceholder =
        showAsPlaceholder && inheritedResult
          ? String(inheritedResult.value)
          : undefined;

      return (
        <div className="flex flex-col gap-1.5">
          <div className="space-y-1">
            <div className="flex items-center gap-2">
              <Label
                htmlFor={fieldId}
                className="text-xs font-medium text-muted-foreground uppercase tracking-wider"
              >
                {label}
              </Label>
              {badge}
            </div>
            {description && (
              <p className="text-xs text-muted-foreground">{description}</p>
            )}
          </div>
          <div>
            <NumericInput
              id={fieldId}
              value={
                showAsPlaceholder
                  ? undefined
                  : typeof value === 'number'
                    ? value
                    : undefined
              }
              onChange={(v) => updateValue(path, v)}
              min={prop.minimum}
              max={prop.maximum}
              integer={prop.type === 'integer'}
              step={prop.multipleOf}
              precision={prop['x-precision']}
              unit={prop['x-unit']}
              inheritedPlaceholder={inheritedPlaceholder}
            />
            <FieldError problem={error} />
            {annotation}
          </div>
        </div>
      );
    }

    // Array (generic)
    if (prop.type === 'array' && prop.items) {
      const items = Array.isArray(value) ? (value as unknown[]) : [];
      const isNestedArray = prop.items.type === 'array';
      const addLabel = isNestedArray
        ? t('plugins.config.addRow')
        : t('plugins.config.addItem');

      return (
        <div className="space-y-1.5">
          <div className="flex items-center gap-2">
            <Label className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
              {label}
            </Label>
            {!hasFieldOverride && <DefaultBadge />}
          </div>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          <div className="space-y-2">
            {/* Index keys: deletion may cause stale NumericInput.text state, acceptable for config forms */}
            {items.map((item, i) => (
              <div key={String(i)} className="flex gap-1.5 items-start">
                <div className="flex-1">
                  <SchemaField
                    name={String(i)}
                    prop={prop.items!}
                    value={item}
                    rootValues={rootValues}
                    path={[...path, String(i)]}
                    updateValue={updateValue}
                    errors={errors}
                    pluginId={pluginId}
                    namespace={namespace}
                    scope={scope}
                    isExplicitValue={isExplicitValue}
                    hasExplicitDescendant={hasExplicitDescendant}
                    isDirty={isDirty}
                    inherited={inherited}
                  />
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="shrink-0 h-9 w-9 text-muted-foreground hover:text-destructive"
                  onClick={() => {
                    updateValue(
                      path,
                      items.filter((_, j) => j !== i),
                    );
                  }}
                >
                  <Minus className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
            <Button
              type="button"
              variant={isNestedArray ? 'secondary' : 'outline'}
              size="sm"
              className="mt-1"
              onClick={() =>
                updateValue(path, [...items, defaultForType(prop.items!)])
              }
            >
              <Plus className="h-3 w-3 mr-1.5" />
              {addLabel}
            </Button>
          </div>
          <FieldError problem={error} />
        </div>
      );
    }

    // Default: string input
    {
      const stringPlaceholder =
        showAsPlaceholder && inheritedResult
          ? String(inheritedResult.value)
          : undefined;

      return (
        <div className="flex flex-col gap-1.5">
          <div className="space-y-1">
            <div className="flex items-center gap-2">
              <Label
                htmlFor={fieldId}
                className="text-xs font-medium text-muted-foreground uppercase tracking-wider"
              >
                {label}
              </Label>
              {badge}
            </div>
            {description && (
              <p className="text-xs text-muted-foreground">{description}</p>
            )}
          </div>
          <div>
            <Input
              id={fieldId}
              value={
                showAsPlaceholder ? '' : typeof value === 'string' ? value : ''
              }
              onChange={(e) => updateValue(path, e.target.value)}
              placeholder={stringPlaceholder}
              minLength={prop.minLength}
              maxLength={prop.maxLength}
            />
            <FieldError problem={error} />
            {annotation}
          </div>
        </div>
      );
    }
  }
}
