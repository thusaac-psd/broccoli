import assert from 'node:assert/strict';
import { test } from 'node:test';

import { fleetFailures, fleetFailureText, type PluginFleet } from './fleet.ts';

const replica = (
  server_id: string,
  status: PluginFleet['replicas'][number]['status'],
  error: string | null = null,
) => ({ server_id, status, error, up_to_date: true });

const fleet = (replicas: PluginFleet['replicas']) =>
  ({ replicas }) as unknown as PluginFleet;

test('the failures group the failing servers by reason, in the fleet order', () => {
  const failures = fleetFailures(
    fleet([
      replica('server-1', 'loaded'),
      replica('server-2', 'failed', 'trap'),
      replica('server-3', 'not_installed'),
      replica('server-4', 'failed', 'trap'),
    ]),
  );
  assert.deepEqual(failures, [
    {
      text: 'plugins.failure.failed',
      servers: ['server-2', 'server-4'],
      errors: ['server-2: trap', 'server-4: trap'],
    },
    {
      text: 'plugins.failure.not_installed',
      servers: ['server-3'],
      errors: [],
    },
  ]);
});

test('a fleet running the plugin everywhere has no failure', () => {
  assert.deepEqual(
    fleetFailures(
      fleet([replica('server-1', 'loaded'), replica('s', 'unloaded')]),
    ),
    [],
  );
});

test('the failure text names the servers of each reason', () => {
  const text = fleetFailureText(
    fleetFailures(
      fleet([
        replica('a', 'failed'),
        replica('b', 'failed'),
        replica('c', 'outdated'),
      ]),
    ),
    (key, { servers }) => `${key}(${servers})`,
    (items) => items.join('+'),
  );
  assert.equal(text, 'plugins.failure.failed(a+b) plugins.failure.outdated(c)');
});
