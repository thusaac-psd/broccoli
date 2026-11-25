import type { I18nContextValue, TranslationKey } from '@/i18n';
import type { ClosedBy, Provided } from '@/submission/types';

import { pluginName } from '../i18n/labels.ts';

/**
 * Where work a plugin holds back is spoken of: a parked submission
 * (`parked_by`), or a problem view that takes no submission (`closed_by`).
 */
export type ClosedByPlace = 'parked' | 'closed';

/**
 * The text of each role a plugin plays for the work, in each place. Every
 * kind `Provided` names has its texts here, so a new kind does not compile
 * until it can be said. Each text takes the plugin's name as `{plugin}`.
 */
const ROLE_TEXT: Record<Provided, Record<ClosedByPlace, TranslationKey>> = {
  contest_type: {
    parked: 'submissions.parkedBy.contest_type',
    closed: 'problem.closedBy.contest_type',
  },
  problem_type: {
    parked: 'submissions.parkedBy.problem_type',
    closed: 'problem.closedBy.problem_type',
  },
  checker_format: {
    parked: 'submissions.parkedBy.checker_format',
    closed: 'problem.closedBy.checker_format',
  },
  language: {
    parked: 'submissions.parkedBy.language',
    closed: 'problem.closedBy.language',
  },
};

/**
 * The text of a provider the work waits for that is enabled but loaded on no
 * server (`reason: 'not_loaded'`), in each place, whatever role it plays.
 */
const NOT_LOADED_TEXT: Record<ClosedByPlace, TranslationKey> = {
  parked: 'submissions.parkedBy.notLoaded',
  closed: 'problem.closedBy.notLoaded',
};

/**
 * What holds work back (`ClosedBy`), in the user's language: the role the
 * plugin plays for the work, or that it is loaded on no server, and the
 * plugin by its translated name
 * (`plugin.<id>.name`), or by its id where no catalog names it, so whoever
 * can enable it knows which plugin to act on. Every view of a parked
 * submission or a closed submit says it through this.
 */
export function closedByText(
  { plugin_id, provides, reason }: ClosedBy,
  place: ClosedByPlace,
  { t, lookup }: Pick<I18nContextValue, 't' | 'lookup'>,
): string {
  const key =
    reason === 'not_loaded'
      ? NOT_LOADED_TEXT[place]
      : ROLE_TEXT[provides][place];
  return t(key, { plugin: pluginName(lookup, plugin_id) });
}
