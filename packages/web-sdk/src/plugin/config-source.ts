import type { ApiSchemas } from '@/api/types';
import type { I18nContextValue } from '@/i18n';

/**
 * Where a plugin's effective config comes from, as the server names it: the
 * most specific scope it takes fields from, the plugin-wide row, the
 * schema's defaults, or the plugin-wide config an override switched off
 * left in place. The server's own type, so a new source cannot pass
 * unlabelled.
 */
export type ConfigSource = ApiSchemas['ConfigSource'];

/**
 * A short label saying where a config shown to a user comes from, in the
 * host's words; `null` for the schema's defaults, which need none.
 */
export function configSourceLabel(
  source: ConfigSource,
  t: I18nContextValue['t'],
): string | null {
  switch (source) {
    case 'contest_problem':
      return t('config.source.contestProblem');
    case 'contest':
      return t('config.source.contest');
    case 'problem':
      return t('config.source.problem');
    case 'plugin':
      return t('config.source.plugin');
    case 'disabled':
      return t('config.source.disabled');
    case 'default':
      return null;
    default:
      return source satisfies never;
  }
}

/**
 * The name of the scope a form field inherits its value from, for its
 * badge ("Contest", "Problem"), in the host's words.
 */
export function configSourceName(
  source: Exclude<ConfigSource, 'default' | 'disabled'>,
  t: I18nContextValue['t'],
): string {
  switch (source) {
    case 'contest_problem':
      return t('config.level.contestProblem');
    case 'contest':
      return t('config.level.contest');
    case 'problem':
      return t('config.level.problem');
    case 'plugin':
      return t('config.level.plugin');
    default:
      return source satisfies never;
  }
}
