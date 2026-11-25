import type {
  ClosedBy,
  ClosedLanguage,
  PluginOutage,
} from '@broccoli/web-sdk/submission';

import type { SupportedLanguage } from './supported-languages.ts';

/** What a problem view reads of a problem to know whether it takes work. */
export interface ProblemSubmitInputs {
  closed_by?: ClosedBy | null;
  closed_languages?: ClosedLanguage[];
  plugin_outage?: PluginOutage | null;
  submission_format?: Record<string, string[]> | null;
}

/**
 * The languages a problem takes work in: `open`, those the editor offers,
 * and `closed`, those the server names as closed by a plugin, which the
 * picker lists as unavailable.
 */
export interface ProblemLanguages {
  open: SupportedLanguage[];
  closed: ClosedLanguage[];
}

/**
 * The languages `problem` takes work in, of those the server can take now
 * (`supported`), by the server's own rule: the ones its submission format
 * names, or every one when it names none. A language the problem names as
 * closed is not open, whatever the registries say, so the two readings
 * disagree only toward a closed submit, never toward code the server
 * refuses.
 */
export function problemLanguages(
  supported: readonly SupportedLanguage[],
  problem: ProblemSubmitInputs | undefined,
): ProblemLanguages {
  const closed = problem?.closed_languages ?? [];
  const closedIds = new Set(closed.map((entry) => entry.language));
  const format = problem?.submission_format;
  const named =
    format && Object.keys(format).length > 0
      ? new Set(Object.keys(format))
      : null;
  const open = supported.filter(
    (language) =>
      !closedIds.has(language.id) && (named === null || named.has(language.id)),
  );
  return { open, closed };
}

/**
 * Whether a problem's languages are worth asking about again: one is closed,
 * or none is open, either of which a plugin being enabled changes.
 */
export function languagesClosed({ open, closed }: ProblemLanguages): boolean {
  return closed.length > 0 || open.length === 0;
}

/**
 * Whether a problem's own answer is worth asking for again: it names a
 * plugin that closes it or one of its languages, or (in practice) its
 * contest type is out of reach. Each ends when a plugin is back.
 */
export function problemClosed(
  scope: 'contest' | 'practice',
  problem: ProblemSubmitInputs | undefined,
): boolean {
  return (
    !!problem?.closed_by ||
    (problem?.closed_languages?.length ?? 0) > 0 ||
    (scope === 'practice' && problem?.plugin_outage?.state === 'unavailable')
  );
}

/**
 * Why a problem view takes no submission now, as one notice says it:
 *
 * - `contest`: its contest's plugin is out, which also withholds the
 *   contest's results.
 * - `closed`: a plugin the submission needs is disabled (`ClosedBy`): its
 *   contest type's, its evaluator, its checker, or the plugin of every
 *   language it takes.
 * - `unavailable`: the practice contest type has no handler on this server
 *   right now; a retry may succeed.
 * - `noLanguage`: no language it takes is available to write in, and no
 *   plugin accounts for it.
 */
export type SubmitClosure =
  | { kind: 'contest'; outage: PluginOutage }
  | { kind: 'closed'; closedBy: ClosedBy }
  | { kind: 'unavailable' }
  | { kind: 'noLanguage' };

/**
 * What closes submitting from a problem view, if anything. Under a contest,
 * `contestOutage` is the contest's `plugin_outage`; on its own, the
 * problem's `plugin_outage` says the same of its practice contest type. The
 * problem's `closed_by` names every plugin the submission needs, in the
 * view's own contest type, so the view shows one notice for any of them.
 * `openLanguages` is how many languages the problem takes that the server
 * can take now (`undefined` until they are known): with none, there is
 * nothing to write in.
 */
export function submitClosure(
  scope: 'contest' | 'practice',
  contestOutage: PluginOutage | null | undefined,
  problem: ProblemSubmitInputs | undefined,
  languages?: ProblemLanguages,
): SubmitClosure | null {
  if (scope === 'contest' && contestOutage) {
    return { kind: 'contest', outage: contestOutage };
  }
  if (problem?.closed_by)
    return { kind: 'closed', closedBy: problem.closed_by };
  if (scope === 'practice' && problem?.plugin_outage?.state === 'unavailable') {
    return { kind: 'unavailable' };
  }
  if (problem && languages?.open.length === 0) {
    const [first] = languages.closed;
    return first
      ? { kind: 'closed', closedBy: first.closed_by }
      : { kind: 'noLanguage' };
  }
  return null;
}
