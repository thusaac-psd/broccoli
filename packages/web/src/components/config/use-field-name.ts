import {
  pluginKeys,
  usePluginText,
  useTranslation,
} from '@broccoli/web-sdk/i18n';
import { useCallback } from 'react';

import type { JsonSchema } from './types';
import { pathSteps } from './utils';

/** Where a config field lives: its plugin, namespace and schema. */
export interface FieldOwner {
  pluginId?: string;
  namespace: string;
  schema: JsonSchema;
}

/**
 * How a config field reads in the UI language: each step of `path` (dotted,
 * `stations.0.token`) by its title in the plugin's pack, an item by its key
 * field's value in `values`, or its number. The settings form's error banner
 * and the config issue lists name fields through this.
 */
export function useFieldName(): (
  owner: FieldOwner,
  path: string,
  values?: Record<string, unknown>,
) => string {
  const { t } = useTranslation();
  const pluginText = usePluginText();
  return useCallback(
    ({ pluginId, namespace, schema }, path, values = {}) =>
      pathSteps(path, schema, values)
        .map((step) =>
          step.kind === 'item'
            ? (step.label ?? t('config.itemNumber', { n: step.index + 1 }))
            : pluginId
              ? pluginText(
                  pluginKeys.configField(
                    pluginId,
                    namespace,
                    step.path,
                    'title',
                  ),
                  step.title,
                )
              : step.title,
        )
        .join(' › '),
    [t, pluginText],
  );
}
