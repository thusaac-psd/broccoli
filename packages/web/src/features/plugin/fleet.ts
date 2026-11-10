import type { PluginDetail } from '@broccoli/web-sdk/plugin';

export type PluginFleet = PluginDetail['fleet'];

/** The first load error any server reports, for a compact hint. */
export function firstFleetError(fleet: PluginFleet): string | undefined {
  return fleet.replicas.find((r) => r.error)?.error ?? undefined;
}
