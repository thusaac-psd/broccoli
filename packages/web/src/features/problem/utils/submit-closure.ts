import type { ClosedBy, PluginOutage } from '@broccoli/web-sdk/submission';

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
 *   disabled plugin accounts for it.
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
  problem:
    | {
        closed_by?: ClosedBy | null;
        plugin_outage?: PluginOutage | null;
      }
    | undefined,
  openLanguages?: number,
): SubmitClosure | null {
  if (scope === 'contest' && contestOutage) {
    return { kind: 'contest', outage: contestOutage };
  }
  if (problem?.closed_by)
    return { kind: 'closed', closedBy: problem.closed_by };
  if (scope === 'practice' && problem?.plugin_outage?.state === 'unavailable') {
    return { kind: 'unavailable' };
  }
  if (problem && openLanguages === 0) return { kind: 'noLanguage' };
  return null;
}
