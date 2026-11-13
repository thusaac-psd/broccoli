import { SchemaField } from './SchemaField';
import type {
  ConfigScope,
  FieldProblem,
  InheritedConfig,
  JsonSchema,
  JsonSchemaProperty,
} from './types';

export function SchemaFields({
  schema,
  values,
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
  schema: JsonSchema;
  values: Record<string, unknown>;
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
  if (!schema.properties) return null;

  // Separate top-level scalars from object groups for better layout
  const entries = Object.entries(schema.properties);
  const isObjectGroup = (prop: JsonSchemaProperty) =>
    prop.type === 'object' && prop.properties;
  const gridFields = entries.filter(([, prop]) => !isObjectGroup(prop));
  const fullWidthFields = entries.filter(([, prop]) => isObjectGroup(prop));

  return (
    <div className="space-y-5">
      {/* Grid fields in a responsive 2-column layout */}
      {gridFields.length > 0 && (
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-5">
          {gridFields.map(([key, prop]) => {
            // Arrays and fields with x-span>=2 span both grid columns
            const wideField =
              (prop['x-span'] != null && prop['x-span'] >= 2) ||
              prop.type === 'array';
            return (
              <div
                key={key}
                className={wideField ? 'sm:col-span-2' : undefined}
              >
                <SchemaField
                  name={key}
                  prop={prop}
                  value={values[key]}
                  rootValues={rootValues}
                  path={[...path, key]}
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
            );
          })}
        </div>
      )}

      {/* Object sections rendered full-width */}
      {fullWidthFields.map(([key, prop]) => (
        <SchemaField
          key={key}
          name={key}
          prop={prop}
          value={values[key]}
          rootValues={rootValues}
          path={[...path, key]}
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
      ))}
    </div>
  );
}
