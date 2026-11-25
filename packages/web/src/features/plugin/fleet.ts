import type { TranslationKey } from '@broccoli/web-sdk/i18n';
import type { PluginDetail } from '@broccoli/web-sdk/plugin';

export type PluginFleet = PluginDetail['fleet'];
export type PluginReplica = PluginFleet['replicas'][number];
type ReplicaStatus = PluginReplica['status'];

/**
 * What each state a replica reports a plugin in means for it, in the
 * reader's language: a failing one says why from its status, the code of the
 * failure, and the server's own words (`error`) are secondary detail. Typed
 * against the API's statuses, so one the server adds fails to compile here.
 */
const FAILURE_TEXT = {
  failed: 'plugins.failure.failed',
  not_installed: 'plugins.failure.not_installed',
  outdated: 'plugins.failure.outdated',
  mismatched: 'plugins.failure.mismatched',
  loaded: null,
  unloaded: null,
} as const satisfies Record<ReplicaStatus, TranslationKey | null>;

/** The translation key of why `replica` fails, or null when it does not. */
export function failureText(replica: PluginReplica): TranslationKey | null {
  return FAILURE_TEXT[replica.status];
}

/** The first server the plugin fails on, for a compact hint. */
export function firstFailure(
  fleet: PluginFleet,
): { replica: PluginReplica; text: TranslationKey } | undefined {
  for (const replica of fleet.replicas) {
    const text = failureText(replica);
    if (text) return { replica, text };
  }
  return undefined;
}
