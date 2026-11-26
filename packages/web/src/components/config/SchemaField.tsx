import {
  pluginKeys,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import { configSourceName } from '@broccoli/web-sdk/plugin';
import {
  InheritedAnnotation,
  InheritedBadge,
  inheritedValueOf,
  Slot,
} from '@broccoli/web-sdk/slot';
import { Button, Input, Label, Switch } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { ArrowDown, ArrowUp, Minus, Plus } from 'lucide-react';
import { use } from 'react';

import { BlobRefField } from './BlobRefField';
import { FieldError } from './FieldError';
import { NumericInput } from './NumericInput';
import { SchemaFields } from './SchemaFields';
import { StoredValues } from './stored-values';
import type {
  ConfigScope,
  FieldProblem,
  InheritedConfig,
  JsonSchemaProperty,
} from './types';
import {
  CLEAR_SECRET_MARKER,
  defaultForType,
  defaultText,
  isClearMarker,
  isNewItem,
  isSecretRef,
  storedRef,
  valueText,
} from './utils';

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
  itemLabel,
  required = false,
  inNewItem = false,
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
  /**
   * The name of an array item, which an item shows in place of the array's
   * title and description: its key field's value, or its number.
   */
  itemLabel?: string;
  /** Whether the object that holds this field requires it. */
  required?: boolean;
  /**
   * Whether the field belongs to an array item added (or re-keyed) since the
   * form loaded: a secret of it has no stored value to keep.
   */
  inNewItem?: boolean;
}>) {
  const { t } = useTranslation();
  const pluginText = usePluginText();
  const itemName = useItemName();
  // The ref the stored secret of this field's own item reads as, found by
  // the item's identity, so emptying the input or keeping it sends that ref
  // back however the rows moved.
  const stored = use(StoredValues);
  const kept = prop.writeOnly ? storedRef(stored, rootValues, path) : null;
  const shownRef = isSecretRef(value) ? value.$secret : null;
  const words = { yes: t('config.yes'), no: t('config.no') };
  // An option of an enum reads as its title, in the plugin's pack.
  const optionText = (option: unknown) => {
    const index = prop.enum?.indexOf(option) ?? -1;
    const plain =
      (index >= 0 ? prop['x-enum-titles']?.[index] : undefined) ??
      valueText(option, words) ??
      String(option);
    return pluginId && namespace
      ? pluginText(
          pluginKeys.configOption(pluginId, namespace, path, String(option)),
          plain,
        )
      : plain;
  };
  const say = (shown: unknown) =>
    prop.enum?.includes(shown) ? optionText(shown) : valueText(shown, words);
  const fieldId = `cfg-${path.join('-')}`;
  // A plugin can name its fields in its own translation pack.
  const label =
    itemLabel ??
    (pluginId && namespace
      ? pluginText(
          pluginKeys.configField(pluginId, namespace, path, 'title'),
          prop.title ?? name,
        )
      : (prop.title ?? name));
  const description =
    itemLabel !== undefined
      ? undefined
      : pluginId && namespace
        ? pluginText(
            pluginKeys.configField(pluginId, namespace, path, 'description'),
            prop.description,
          )
        : prop.description;
  const unit =
    pluginId && namespace && prop['x-unit']
      ? pluginText(
          pluginKeys.configField(pluginId, namespace, path, 'unit'),
          prop['x-unit'],
        )
      : prop['x-unit'];
  const dotPath = path.join('.');
  const error = errors[dotPath];
  const isFieldExplicit = isExplicitValue(path);
  const hasFieldOverride = hasExplicitDescendant(path);
  const isFieldDirty = isDirty?.(path) ?? false;

  // The server says where each top-level field comes from.
  const inheritedValue =
    path.length === 1 ? inheritedValueOf(path[0], inherited) : null;
  const inheritedResult = inheritedValue && {
    value: inheritedValue.value,
    // A value from a scope the viewer may not read is shown as withheld.
    display: inheritedValue.withheld
      ? t('config.withheldValue')
      : (say(inheritedValue.value) ?? ''),
    source: configSourceName(inheritedValue.source, t),
  };
  const showAsPlaceholder = !isFieldExplicit && !isFieldDirty;
  // What an unset field shows, for every kind of field: the value it
  // inherits, else its default.
  const unsetText = showAsPlaceholder
    ? (inheritedResult?.display ?? defaultText(prop, path, say, inherited))
    : undefined;

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
        value={inheritedResult.display}
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
          label,
          description,
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
              schema={{
                type: 'object' as const,
                properties: prop.properties,
                required: prop.required,
              }}
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
              inNewItem={inNewItem}
            />
          </div>
        </div>
      );
    }

    // Boolean -> horizontal settings-style card with switch on the right
    if (prop.type === 'boolean') {
      const inheritedBoolLabel =
        unsetText === undefined ? undefined : `(${unsetText})`;

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
                <span className="text-[10px] text-muted-foreground italic">
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
      const selectPlaceholder = unsetText;

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
                  {optionText(opt)}
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
      const inheritedPlaceholder = unsetText;

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
              unit={unit}
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
                    itemLabel={itemName(item, prop['x-key'], i)}
                    inNewItem={inNewItem || isNewItem(item, prop['x-key'])}
                  />
                </div>
                {/* An item moves with everything it holds, its secrets too:
                    the server matches a kept secret to its item by key. */}
                <Button
                  aria-label={t('config.moveUp')}
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="shrink-0 h-9 w-9 text-muted-foreground"
                  disabled={i === 0}
                  onClick={() => updateValue(path, swapped(items, i - 1, i))}
                >
                  <ArrowUp className="h-3.5 w-3.5" />
                </Button>
                <Button
                  aria-label={t('config.moveDown')}
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="shrink-0 h-9 w-9 text-muted-foreground"
                  disabled={i === items.length - 1}
                  onClick={() => updateValue(path, swapped(items, i, i + 1))}
                >
                  <ArrowDown className="h-3.5 w-3.5" />
                </Button>
                <Button
                  aria-label={t('common.remove')}
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

    // Default: string input. A secret is never read back: a stored one
    // reads as its ref, which an empty input writes back to keep it.
    // Clearing it sends the explicit marker.
    {
      const secret = prop.writeOnly === true;
      // A stored secret reads as its ref; one of an item added or renamed
      // since the form loaded has no stored value, whatever it holds.
      const keeps = secret && !inNewItem && kept !== null;
      const clearing = secret && isClearMarker(value);
      const stringPlaceholder = clearing
        ? t('config.secretCleared')
        : keeps
          ? t('config.secretKept')
          : unsetText;

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
            <div className="flex gap-1.5">
              <Input
                id={fieldId}
                value={
                  showAsPlaceholder
                    ? ''
                    : typeof value === 'string'
                      ? value
                      : ''
                }
                onChange={(e) =>
                  updateValue(
                    path,
                    secret && e.target.value === ''
                      ? keeps
                        ? kept
                        : null
                      : e.target.value,
                  )
                }
                type={secret ? 'password' : undefined}
                autoComplete={secret ? 'off' : undefined}
                placeholder={stringPlaceholder}
                minLength={prop.minLength}
                maxLength={prop.maxLength}
              />
              {/* A secret its item requires is replaced, never cleared. */}
              {keeps && shownRef !== null && !required && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="h-9 shrink-0"
                  onClick={() => updateValue(path, { ...CLEAR_SECRET_MARKER })}
                >
                  {t('config.clearSecret')}
                </Button>
              )}
              {clearing && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="h-9 shrink-0"
                  onClick={() => updateValue(path, keeps ? kept : null)}
                >
                  {t('config.keepSecret')}
                </Button>
              )}
            </div>
            <FieldError problem={error} />
            {annotation}
          </div>
        </div>
      );
    }
  }
}

/**
 * What an array item is called: the value of its array's key field, or
 * "Item N" when it has none yet.
 */
function useItemName() {
  const { t } = useTranslation();
  return (item: unknown, key: string | undefined, index: number) => {
    const named =
      key && item && typeof item === 'object'
        ? (item as Record<string, unknown>)[key]
        : undefined;
    return typeof named === 'string' && named !== ''
      ? named
      : t('config.itemNumber', { n: index + 1 });
  };
}

/** `items` with the items at `a` and `b` swapped. */
function swapped<T>(items: readonly T[], a: number, b: number): T[] {
  const next = [...items];
  [next[a], next[b]] = [next[b], next[a]];
  return next;
}
