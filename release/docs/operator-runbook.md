# Operator Runbook

Run commands on the machine that owns the role.

The role compose files use per-role env files. If bypassing `install.sh`, copy
or create the matching `.env.<role>` first and always pass
`--env-file .env.<role>` to Docker Compose.

## Status

Infra node:

```bash
docker compose --env-file .env.infra -f docker-compose.infra.yaml ps
docker compose --env-file .env.infra -f docker-compose.infra.yaml logs --tail=200 db redis seaweedfs seaweedfs-init
```

Server node:

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml ps
set -a; . ./.env.server; set +a
curl -fsS http://127.0.0.1:${BROCCOLI_HTTP_BIND##*:}/healthz
docker compose --env-file .env.server -f docker-compose.server.yaml logs --tail=200 server
```

Worker node:

```bash
docker compose --env-file .env.worker -f docker-compose.worker.yaml ps
docker compose --env-file .env.worker -f docker-compose.worker.yaml logs --tail=200 worker
```

Gateway node:

```bash
docker compose --env-file .env.gateway -f docker-compose.gateway.yaml ps
set -a; . ./.env.gateway; set +a
curl -fsS http://127.0.0.1:${BROCCOLI_GATEWAY_HTTP_BIND##*:}/healthz
docker compose --env-file .env.gateway -f docker-compose.gateway.yaml logs --tail=200 gateway
```

## Restart

Restart one role at a time:

```bash
docker compose --env-file .env.worker -f docker-compose.worker.yaml restart worker
docker compose --env-file .env.server -f docker-compose.server.yaml restart server
docker compose --env-file .env.gateway -f docker-compose.gateway.yaml restart gateway
```

For server redundancy, restart one server machine at a time and confirm the
gateway stays healthy before moving to the next server. Avoid restarting infra
during a contest window.

## Plugin Reload

The server image contains the default plugins and also mounts `./plugins`. To
roll out new plugin files:

1. Copy the same files to `./plugins` on **every** server node.
2. Select **Reload** for the plugin (or **Reload All**) once, on the admin
   Plugins page. The server that handles the click loads the new files first;
   only if that works is the change published, and every other server reloads
   from its own `./plugins` within a few seconds.

The Plugins page shows each server's state. A server that cannot load the new
version keeps running the version it had and shows **Running previous version**;
a server whose files differ from the published ones shows **Different files**.
Fix that node's files and select **Retry** (or **Reload**) again. Enable,
disable and reload all act on every server; there is no need to restart servers
or repeat the action per node.

Upgrade all server nodes together. Servers from before this mechanism do not
publish plugin changes to their peers, so in a mixed fleet a change made through
an old server only reaches that server.

## Password Reset

Use the admin UI when another admin account is available. If all admin accounts
are locked out, edit the server node `.env.server` and restart the server:

```bash
BROCCOLI_BOOTSTRAP_ADMIN_USERNAME=admin
BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD=replace-this-password
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
```

Remove or rotate the bootstrap password after login.

## Logs

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml logs -f server
docker compose --env-file .env.worker -f docker-compose.worker.yaml logs -f worker
docker compose --env-file .env.infra -f docker-compose.infra.yaml logs -f db redis seaweedfs
```

For worker sandbox failures, first confirm the worker service is privileged and
that the worker host uses cgroup v2.

## Storage Checks

The recommended (and default) backend is `object_storage`. The `database`
backend exists for tiny demos with no S3 available, but cannot survive even a
50-submission burst because every blob fetch holds a Postgres connection for the
entire stream.

If `BROCCOLI__STORAGE__BACKEND=object_storage`, SeaweedFS runs on the infra
node. Confirm the bucket exists:

```bash
docker compose --env-file .env.infra -f docker-compose.infra.yaml run --rm seaweedfs-init
```

Server and worker nodes must use the same
`BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT`,
`BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY`, and
`BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY`.

## LAN Credential Files

Infra and single-host installs generate two copyable files:

```bash
connection.env       # copy to servers and workers
server-secrets.env   # copy only to servers
```

Workers do not need `server-secrets.env`.

## Capacity Planning

Broccoli's per-machine throughput is bounded by the worker's intra-process
concurrency (`worker.max_concurrency`, default 1). Total fleet drain time for a
contest:

```
drain_seconds = (n_submissions * n_testcases * avg_op_time_seconds)
              / (n_machines * worker.max_concurrency)
```

For 100 submissions × 50 testcases × 5 s on 7 machines × 4 slots ≈ 30 minutes.

Servers default to local evaluator admission sizing. After worker heartbeats are
visible and include `worker.max_concurrency`, operators can enable
`BROCCOLI__SERVER__FLEET_AWARE_ADMISSION_ENABLED=true`; each server then polls
live worker/server heartbeats and sizes its evaluator slots to roughly
`sum(live worker max_concurrency) / live server replicas`.

**Deployment topology**

The official deployment model is **one worker process per physical machine**,
with the worker's own `max_concurrency` knob (default 1) controlling how many
sandbox slots run in parallel inside that single daemon.

Scale the fleet by adding machines, not by running multiple worker daemons on
the same box. Multiple processes per box would contend for CPU cache, memory
bandwidth, and SMT siblings, and the verdict signal Broccoli currently uses
(`time_used`, which is kernel CPU accounting) inflates under that contention, so
borderline TLE outcomes can flip. The single-daemon model gives the operator one
number to reason about and one place to apply core-pinning / cgroup
configuration.

Raising `max_concurrency` above 1 within a single daemon has the same fairness
trade-off. Use the procedure below before changing it.

**Raising max_concurrency safely**

Default worker concurrency is `max_concurrency=1`. Leave it there unless the
worker host has been prepared for CPU isolation.

On the Linux worker host, run:

```bash
sudo ./release/setup-fairness.sh --max-concurrency 4 --cpus 0-3
cat fairness.env >> .env.worker
docker compose --env-file .env.worker -f docker-compose.worker.yaml up -d --no-deps worker
```

The script refuses to run when SMT/hyperthreading is enabled unless
`--allow-smt` is passed. It sets CPU governors to `performance` when the host
exposes writable governor controls, configures isolate per-box CPU assignments
so each sandbox `box_id` writes a fixed `cpuset.cpus`, and writes the worker env
snippet:

```bash
BROCCOLI__WORKER__MAX_CONCURRENCY=4
BROCCOLI__WORKER__FAIRNESS_UNSAFE_ALLOW=false
```

WSL and macOS are not supported production fairness targets. Use WSL only for
local isolate smoke tests and keep `BROCCOLI__WORKER__MAX_CONCURRENCY=1`; macOS
cannot run the Linux cgroup setup and should also stay clamped to 1.

Phase 1 only makes multi-slot judging an explicit operator opt-in. It does not
prove verdict fairness: instruction-count instrumentation with `perf_event_open`
is Phase 5. Treat any higher concurrency as an operational risk for borderline
time-limit cases until Phase 5 data exists.

**Sizing the database connection pool**

Postgres `max_connections` must accommodate every server replica's pool, every
worker's pool, plus heartbeat/DLQ/admin overhead. The tuned infra template
defaults to `max_connections=400`, which fits:

```
3 servers × 50 + 7 workers × 5 + heartbeat refreshers + DLQ + admin
≈ 215 client demand
```

Override via `POSTGRES_MAX_CONNECTIONS` in `.env.infra` if your fleet grows.

**Sizing Redis memory**

The tuned infra template defaults `--maxmemory 6gb`, sized for a 1000-submission
spike with four active operations per submission. Override via
`REDIS_MAXMEMORY`. Tiers:

| Deployment                     | Recommended `REDIS_MAXMEMORY` |
| ------------------------------ | ----------------------------- |
| Single-host / small LAN demo   | `2gb`                         |
| Standard contest (≤1000 spike) | `6gb` (default)               |
| Large/national (≥5000 spike)   | `12gb`                        |

**Why `--maxmemory-policy noeviction` is mandatory**

Every key Broccoli writes to Redis is correctness-bearing: MQ payloads are
queued submissions, heartbeats drive worker-dedup steal logic, dedup keys carry
claim invariants, and the planned observability streams (see
`docs/plans/2026-05-07-observability-expansion-design.md`) hold live SSE state
admins watch. `allkeys-lru` would silently lose all of these. With `noeviction`,
a full Redis returns `OOM command not allowed`; workers retry, the API returns
`503 OVERLOADED`, and operators get an alert — fail-loud beats fail-silent.
Worker dedup keys expire via `BROCCOLI__WORKER__DEDUP_TTL_SECS` (default 600)
instead of the DLQ stuck-job timeout.

## Plugin Pool Sizing

The server holds per-plugin pools of WASM instances. Each parallel test-case
evaluation acquires (1) an evaluator-semaphore slot and (2) one instance from
the evaluator plugin's pool, then optionally re-enters the host to call the
checker plugin, which acquires one instance from the checker plugin's pool.

**The constraint:**

```
BROCCOLI__PLUGIN__POOL_MAX_INSTANCES >= BROCCOLI__PLUGIN__EVALUATOR_PARALLELISM
```

The server enforces this at startup: if `pool_max_instances` is smaller, it is
raised to match `evaluator_parallelism` and a warning is logged
(`pool_max_instances < evaluator_parallelism; raising pool size to match`). To
silence the warning, set `BROCCOLI__PLUGIN__POOL_MAX_INSTANCES` explicitly.

**Why it matters:**

- For independent plugins (e.g. `icpc-contest` calling `standard-checkers`),
  violating the constraint is **not** a deadlock — but the evaluator semaphore
  ends up sized larger than actual capacity, so excess admitted callers queue
  inside `plugin_manager.call_raw` waiting for a pool slot. Symptom:
  `plugin_instance_acquire_duration` climbs, latency tails fan out, and the
  fleet capacity calculation overestimates throughput.
- For self-checking plugins (a contest plugin that re-enters its own pool to
  invoke a checker handler), violating the constraint **is** a deadlock: every
  pool slot is held by a caller waiting for another slot. Auto-bump prevents
  this case at startup.

Both knobs are per-server-replica.

- `pool_max_instances` (default 32, but auto-raised to `evaluator_parallelism`
  at startup if smaller) caps concurrent calls into any single plugin. Every
  loaded plugin gets its own pool of this size.
- `evaluator_parallelism` (default = `std::thread::available_parallelism()`) is
  the maximum number of test-case evaluations a server admits concurrently.
  Raise it for contest workloads with high per-submission test-case fan-out.

For a typical contest workload on a high-core box, set both explicitly so the
auto-bump warning stays quiet and intent is obvious:

```
BROCCOLI__PLUGIN__POOL_MAX_INSTANCES=64
BROCCOLI__PLUGIN__EVALUATOR_PARALLELISM=64
```

`release/.env.server.example` ships these as the multi-replica defaults.

**Diagnosing pool contention.** Both the deadlock case and the queueing case
show up as climbing `plugin_instance_acquire_duration` for the bottlenecked
plugin and flat `plugin_instance_acquire_failures` (callers wait rather than
fail). When the fleet-aware admission semaphore saturates, the API returns
`503 OVERLOADED`. The auto-bump removes the misconfigured-pool failure mode;
remaining contention is genuine capacity exhaustion and requires either more
replicas or higher `pool_max_instances` (at the cost of WASM instance memory).

The longer-term fix is to move checker invocation out of the contest plugin call
path (so the contest call returns before the checker runs), which makes the
constraint disappear. That refactor is tracked as a follow-up and is not part of
Phase 1.

## Multi-Replica Server Identity

`BROCCOLI__SERVER__ID` **must** be set explicitly on every server replica in a
multi-server deployment. The release templates set it (`.env.server.example`
ships `BROCCOLI__SERVER__ID=server-1`); confirm each replica's `.env.server` has
a unique stable value (`server-1`, `server-2`, `server-3`, ...).

**Why it matters:** each server consumes its own per-replica result queue
(`operation_results.<server_id>`). If two replicas share the same ID, they will
race to consume each other's results. If a replica's ID is randomized at startup
(the fallback when `BROCCOLI__SERVER__ID` is empty and the OS hostname is
unsuitable), every restart leaves a permanent ghost queue in Redis that fills
memory until manually deleted.

The release `install.sh` prompts for the ID; if you bypass the installer, audit
each replica's env file before starting.

## Recovering In-Flight Submissions After a Server Restart or Crash

Every server re-dispatches its own unfinished work automatically, whatever
`BROCCOLI__SERVER__DISPATCHER_LEASE_STEAL_ENABLED` is set to:

- **Graceful stop** (`docker compose stop`, a rolling restart, a redeploy): the
  server releases the submissions, rejudges and code runs it was judging as it
  shuts down, and the other servers pick them up within one steal-scan interval
  (`steal_scan_interval_secs`, default 15 s). Nobody has to resubmit.
- **Restart after a crash**: a server that comes back with the same
  `BROCCOLI__SERVER__ID` releases and re-dispatches the work its previous
  instance left behind as it boots. That work no longer counts against its
  in-flight limit.
- **Crash without a restart**: with lease/steal enabled, the other servers steal
  the dead server's work once its lease expires (`lease_ttl_secs`). With it
  disabled (the release default) nothing can tell a dead server from a live one,
  so the work waits until that server ID boots again, or until the stuck-job
  detector fails it after `max_inflight_secs` (default 1 hour) so it can be
  rejudged.

When a crashed server will not come back:

1. Confirm the failed server is dead (not merely network-partitioned). Two
   replicas with the same ID racing on the same queue is worse than ghost
   queues.
2. Either start a replacement with the same `BROCCOLI__SERVER__ID` (its boot
   recovery re-dispatches the stranded work), or rejudge the affected
   submissions once the stuck-job detector has failed them.
3. If `redis-cli SCAN MATCH operation_results.*` shows queues with no live
   consumer (i.e. their `<server_id>` does not match any healthy replica),
   `redis-cli DEL <queue_name> <queue_name>_processing <queue_name>_failed <queue_name>_fairness_set`
   to reclaim the memory.
