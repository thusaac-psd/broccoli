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

/** The servers that fail to run a plugin for one reason. */
export interface FleetFailure {
  /** Why, as a translation taking the servers (`{servers}`). */
  text: TranslationKey;
  /** The failing servers, by `server_id`, in the fleet's order. */
  servers: string[];
  /** The servers' own words, `server_id: error`, as secondary detail. */
  errors: string[];
}

/**
 * Where and why the plugin fails, one entry per reason in the order the
 * fleet first reports it, each naming its servers: the server that answered
 * may be one the plugin runs on, so nothing says "this server".
 */
export function fleetFailures(fleet: PluginFleet): FleetFailure[] {
  const byText = new Map<TranslationKey, FleetFailure>();
  for (const replica of fleet.replicas) {
    const text = failureText(replica);
    if (!text) continue;
    let failure = byText.get(text);
    if (!failure) {
      failure = { text, servers: [], errors: [] };
      byText.set(text, failure);
    }
    failure.servers.push(replica.server_id);
    if (replica.error)
      failure.errors.push(`${replica.server_id}: ${replica.error}`);
  }
  return [...byText.values()];
}

/** The failures said as one text, each reason naming its servers. */
export function fleetFailureText(
  failures: readonly FleetFailure[],
  t: (key: TranslationKey, params: { servers: string }) => string,
  list: (items: readonly string[]) => string,
): string {
  return failures
    .map((failure) => t(failure.text, { servers: list(failure.servers) }))
    .join(' ');
}
