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

### Health probes and metrics

Each server answers `/livez` (the process is up), `/healthz` (it can also reach
Postgres and Redis) and `/metrics` on its main HTTP port. These share the
runtime that serves API requests and judging, so a server saturated by a
submission burst can answer a probe slowly and fail it even though it is still
working. The gateway probes `/livez` every 5 s with a 2 s timeout and ejects a
server after 2 misses (`health_interval`, `health_timeout` and `health_fails` in
`examples/Caddyfile.gateway`, and in `airgap/caddy/Caddyfile.airgap` for an
airgap install). If servers are ejected under load while their logs show them
still serving, raise `health_timeout` or `health_fails` there and restart the
gateway. A Prometheus scrape of `/metrics` can likewise time out under load;
give it a scrape timeout of a few seconds.

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

## Plugin Updates

A plugin runs from one of two sources, shown on the admin Plugins page:

- **Bundled**: the files in `./plugins` on each node (mounted read-only).
- **Uploaded**: an archive published through the admin UI, stored in the shared
  blob storage. It takes precedence over a bundled copy of the same plugin.

### Uploading

1. On the admin Plugins page select **Upload plugin** and choose the archive: a
   `.tar.gz` with one top-level folder named after the plugin id, containing
   `plugin.toml` and the files it names (build output, not sources).
2. Check the id, version, the version it replaces and the host permissions it
   requests, then select **Publish**.
3. The server that handles the click validates the archive, stores it, and loads
   it. Only if that load works is it published; otherwise the page shows the
   load error and nothing changes anywhere. Every other server then fetches it,
   checks its hash, and loads it within a few seconds.

Each server unpacks uploads into its own `plugin_cache` volume
(`BROCCOLI__PLUGIN__CACHE_DIR`). It is a cache: deleting it costs one download
per uploaded plugin on the next start. Archives are limited to 256 MiB
compressed and 512 MiB unpacked.

To roll back, upload the previous archive again, or open the plugin's details
and select **Remove upload**: every server returns to the bundled version, or,
for a plugin that is not bundled, the plugin is removed from every server (its
configuration is kept for a later upload).

Set `BROCCOLI__PLUGIN__UPLOADS_ENABLED=false` on every server to switch uploads
off. Upload requests are then refused, and a server refuses to load an uploaded
version, so it runs only bundled code; a plugin whose published version is an
upload shows as failed on that server until the upload is removed.

### Updating bundled files

1. Copy the same files to `./plugins` on **every** server node.
2. Select **Reload** for the plugin (or **Reload All**) once, on the admin
   Plugins page. The server that handles the click loads the new files first;
   only if that works is the change published, and every other server reloads
   from its own `./plugins` within a few seconds.

A plugin with an uploaded version keeps running the upload; remove the upload to
run the bundled files.

### When a server lags

The Plugins page shows each server's state. A server that cannot load the new
version keeps running the version it had and shows **Running previous version**;
a server whose files differ from the published ones shows **Different files**.
Fix the cause (that node's files, or the blob storage for an upload) and select
**Retry** (or **Reload**) again. Enable, disable, reload, upload and remove act
on every server; there is no need to restart servers or repeat the action per
node.

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

Each server sizes its plugin execution from its own cores and memory (see the
plugin execution section below); nothing in this formula needs a server setting.

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

Reply streams and the dead-letter stream are capped at about 100 000 entries
each, so a stuck server or a long database outage cannot fill Redis. The shared
operation stream is not capped (trimming it would drop queued work): watch
`broccoli_mq_queue_depth{queue_role="operation"}`; the server logs an error when
it crosses 50 000.

```bash
redis-cli -a "$REDIS_PASSWORD" XLEN operation_tasks              # unread plus in flight
redis-cli -a "$REDIS_PASSWORD" XPENDING operation_tasks workers  # in flight, per worker
redis-cli -a "$REDIS_PASSWORD" XLEN operation_tasks:dlq          # dead letters not yet recorded
```

## Plugin Execution

Every plugin call is admitted before it runs. A server holds two CPU tokens per
core; a call takes one before it gets a thread and gives it back while it waits
in a host function (a database query, a judging result, a call into another
plugin). Half the tokens are guaranteed to interactive calls (visibility,
request hooks, plugin routes) and half to background calls (judging, operations,
timers); either side borrows the other's idle tokens. Plugin instances are built
on demand and dropped, least recently used first, when the process's resident
memory passes its budget.

There are three settings, and all of them size themselves:

| Setting                                  | Default                                                                         |
| ---------------------------------------- | ------------------------------------------------------------------------------- |
| `BROCCOLI__PLUGIN__MAX_CONCURRENT_CALLS` | two per available core                                                          |
| `BROCCOLI__PLUGIN__MEMORY_BUDGET`        | 3/4 of the container's memory limit, or 1/4 of the machine's memory without one |
| `BROCCOLI__PLUGIN__CALL_TIMEOUT_SECS`    | 300                                                                             |

Leave them unset unless a measurement says otherwise. A machine shared with
Postgres, Redis or a worker without a container memory limit should get an
explicit `MEMORY_BUDGET` (for example `4GiB`).

**When it is busy.** A request whose plugin cannot answer before the request's
deadline gets `503 PLUGIN_UNAVAILABLE` with `Retry-After`; the web client shows
it as a temporary state and retries. It is never shown as a missing page or an
empty list. Judging queues longer: a judging call waits for a token up to
`CALL_TIMEOUT_SECS`, and one still waiting then ends the judgement in a system
error that the server rejudges.

**What to watch.** `broccoli_plugin_admission_queued_total` counts calls that
found no free token, `broccoli_plugin_admission_wait_duration_seconds` how long
they waited (`outcome="expired"` means they gave up at their deadline),
`broccoli_plugin_calls_in_flight` and `broccoli_plugin_instances_live` show the
current load per plugin, and `broccoli_plugin_instance_evicted_total` how often
the memory budget dropped instances. Sustained queueing with every token busy is
CPU exhaustion: add server replicas.

A removed key (`POOL_MAX_INSTANCES`, `EVALUATOR_PARALLELISM`,
`SERVER__MAX_BLOCKING_THREADS` and others) still set in the server's config file
or its process environment logs a warning at boot naming each one; the keys are
ignored. The compose template passes the server only the variables it lists, so
an old value left in `.env` never reaches the server and logs nothing: delete
it.

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

**Why it matters:** each server reads its own reply stream
(`operation_tasks:reply:<server_id>`) and owns in-flight work under its ID. A
second process with the same ID refuses to start. A replica whose ID changes at
every restart (the random fallback when no stable ID can be derived) leaves its
old reply stream behind until it expires, 10 minutes later.

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
