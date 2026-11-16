import type { MessageKey } from '@/lib/i18n/catalog';

export interface JsonSchemaProperty {
  type?: string;
  title?: string;
  description?: string;
  default?: unknown;
  minimum?: number;
  maximum?: number;
  minLength?: number;
  maxLength?: number;
  enum?: unknown[];
  format?: string;
  items?: JsonSchemaProperty;
  properties?: Record<string, JsonSchemaProperty>;
  required?: string[];
  multipleOf?: number;
  'x-precision'?: number;
  'x-unit'?: string;
  'x-span'?: number;
}

export interface JsonSchema extends JsonSchemaProperty {
  type: 'object';
  properties?: Record<string, JsonSchemaProperty>;
}

export type ConfigScope =
  | { scope: 'plugin'; pluginId: string }
  | { scope: 'contest'; contestId: number }
  | { scope: 'problem'; problemId: number }
  | { scope: 'contest_problem'; contestId: number; problemId: number };

export type {
  InheritedConfig,
  InheritedValue,
  ParentScopeConfig,
} from '@broccoli/web-sdk/slot';

/** A field's validation problem: an i18n key and its parameters. */
export interface FieldProblem {
  key: MessageKey;
  params?: Record<string, string | number>;
}
