import type { I18nContextValue } from '@/i18n';

/**
 * Where a plugin's effective config comes from, as the server SDK's
 * `ConfigSource` names it: the most specific scope it takes fields from,
 * the plugin-wide row, the schema's defaults, or the plugin-wide config an
 * override switched off left in place.
 */
export type ConfigSource =
  | 'contest_problem'
  | 'contest'
  | 'problem'
  | 'plugin'
  | 'default'
  | 'disabled';

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
  }
}
