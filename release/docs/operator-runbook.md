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

Use the admin UI when another admin account is available.

The bootstrap admin (`BROCCOLI_BOOTSTRAP_ADMIN_USERNAME`) is created on the
first boot. After that, a restart leaves it alone: a password changed in the UI,
an `admin` role revoked on purpose, or the account renamed or deleted, survives
restarts. A boot creates the account (if no active user has that username) or
resets it to the configured password, and restores the `admin` role and its
built-in permissions, only in two cases:

- no active user holds `system:admin`; this happens automatically;
- `BROCCOLI_BOOTSTRAP_ADMIN_RESET=true` is set; use this when admin accounts
  still exist but nobody can sign in to one.

To recover, edit the server node `.env.server` and restart the server:

```bash
BROCCOLI_BOOTSTRAP_ADMIN_USERNAME=admin
BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD=replace-this-password
BROCCOLI_BOOTSTRAP_ADMIN_RESET=true
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
```

The reset also signs the account out everywhere. After signing in, remove
`BROCCOLI_BOOTSTRAP_ADMIN_RESET` (otherwise every restart resets the password
again) and rotate the bootstrap password.

## Built-in Role Permissions

Each built-in role permission (for example `submission:submit` on `contestant`)
is granted once, on the first boot that knows about it. Revoking one through the
API or UI persists across restarts. A permission that a later release adds to a
built-in role is granted once on the first boot of that release. The recovery
above re-grants the `admin` role's built-in permissions.

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

## Time Sync on an Offline LAN

Every server and worker must agree on the time to within about a second. The
message queue orders messages by the publisher's clock, so a server running
ahead of a worker by some delta hides its work from that worker for that delta
(and the other way round for results); a few seconds of skew add seconds to
every test case, and hours of skew lose results. Worker liveness (10 s),
refresh-token reuse and lease checks also compare clocks across machines. An
offline venue has no internet NTP, so make the infra node the LAN's time server
with chrony and point every other node at it. Containers use their host's clock,
so this is host configuration.

On the infra node, set the clock once (for example from a phone:
`sudo date -s '2026-11-10 09:00:00'`), then in `/etc/chrony/chrony.conf`
(`/etc/chrony.conf` on RHEL-family hosts):

```
# Serve this machine's clock to the LAN even without an upstream source.
local stratum 8
allow 10.0.0.0/24        # the venue LAN
```

On every server and worker node, replace the `pool`/`server` lines with:

```
server 10.0.0.10 iburst  # the infra node
makestep 1 -1            # step the clock whenever it is more than 1 s off
```

Restart chrony (`sudo systemctl restart chrony`, or `chronyd`) everywhere, then
check each node: `chronyc tracking` names the infra node as its reference and
shows a system time offset well under a second, and `chronyc sources` marks it
with `*`. Pointing contestant PCs at the same server also keeps contest
countdowns and certificate validity honest.

`install.sh` reports the platform time-sync status and, when online, compares
the clock against an HTTPS `Date` header. Both checks only warn, and offline the
HTTPS check gives up after three seconds, so leave it on for real events.
`BROCCOLI_SKIP_TIME_CHECK=1` skips it entirely and exists for scripted test
runs.

## Capacity Planning

Broccoli's per-machine throughput is bounded by the worker's intra-process
concurrency (`worker.max_concurrency`, default 1). Total fleet drain time for a
contest:

```
drain_seconds = (n_submissions * n_testcases * avg_op_time_seconds)
              / (n_machines * worker.max_concurrency)
```

For 100 submissions × 50 testcases × 5 s on 7 machines × 4 slots ≈ 30 minutes.

Each server sizes its evaluator admission locally, from
`BROCCOLI__PLUGIN__EVALUATOR_PARALLELISM` (see the plugin pool section below).

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

On the Linux worker host, from the bundle directory, run:

```bash
sudo ./setup-fairness.sh --max-concurrency 4 --cpus 0-3
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

Every key Broccoli writes to Redis is correctness-bearing: message queue stream
entries are queued or in-flight operations and dead letters, and node liveness
keys decide which servers and workers are alive. `allkeys-lru` would silently
lose them. With `noeviction`, a full Redis returns `OOM command not allowed`:
publishes fail, the operation batch fails loudly, and workers retry the replies
they could not deliver. Acknowledged tasks are deleted at once and each server
trims its reply stream as it reads it, so memory tracks the live backlog.

Redis 7 or newer is required. Keep `--appendonly yes` with
`--appendfsync everysec`: a Redis crash then loses at most the last second of
writes, and the stuck-job detector recovers the work lost in it. A worker that
crashes loses nothing: the tasks it held are redelivered to another worker 30 to
35 seconds after it stopped refreshing them.

Useful checks on the infra node (`operation_tasks` is
`mq.operation_queue_name`):

```bash
redis-cli -a "$REDIS_PASSWORD" XLEN operation_tasks              # unread plus in flight
redis-cli -a "$REDIS_PASSWORD" XPENDING operation_tasks workers  # in flight, per worker
redis-cli -a "$REDIS_PASSWORD" XLEN operation_tasks:dlq          # dead letters not yet recorded
```

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

`docker-compose.server.yaml.template` passes both to every server, with these
values as its defaults; set them in `.env.server` to change them. To size
tokio's blocking pool by hand, set `BROCCOLI__SERVER__MAX_BLOCKING_THREADS`
there too; when it is unset the server picks a value from the CPU count.

**Diagnosing pool contention.** Both the deadlock case and the queueing case
show up as climbing `plugin_instance_acquire_duration` for the bottlenecked
plugin and flat `plugin_instance_acquire_failures` (callers wait rather than
fail). The auto-bump removes the misconfigured-pool failure mode; remaining
contention is genuine capacity exhaustion and requires either more replicas or
higher `pool_max_instances` (at the cost of WASM instance memory).

The longer-term fix is to move checker invocation out of the contest plugin call
path (so the contest call returns before the checker runs), which makes the
constraint disappear. That refactor is tracked as a follow-up and is not part of
Phase 1.

## Multi-Replica Server Identity

`BROCCOLI__SERVER__ID` **must** be set explicitly on every server replica in a
multi-server deployment, and the same holds for `BROCCOLI__WORKER__ID` on every
worker. `install.sh server` and `install.sh worker` default each to an id unique
to the machine, such as `worker-ubuntu-3f9c2a1b`: the hostname plus a hash of
the host's `/etc/machine-id` and the MAC address of its first physical network
card, so cloned or live-boot machines that all call themselves `ubuntu` still
get distinct ids. The installer writes the id into `.env.server` or
`.env.worker`, and re-runs keep it. If you write env files by hand
(`.env.server.example` ships `server-1`), give each replica a unique stable
value.

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
   replicas with the same ID refuse to run side by side, but a partitioned one
   still holds its work.
2. Either start a replacement with the same `BROCCOLI__SERVER__ID` (its boot
   recovery re-dispatches the stranded work), or rejudge the affected
   submissions once the stuck-job detector has failed them.

A dead server's reply stream needs no cleanup: it expires 10 minutes after the
server stopped.
