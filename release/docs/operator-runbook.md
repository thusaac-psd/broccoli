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

Each server answers three probes and `/metrics` on its main HTTP port:

- `/livez`, liveness: the process is up and serving HTTP. It calls nothing else.
- `/readyz`, readiness: this server should take browsers. A server is ready
  unless it lacks an enabled plugin that another live server serves, whatever
  the plugin provides (a contest type, a language, an evaluator, a checker, a
  page or route). A server counts as serving a plugin only in the version the
  fleet published, not an outdated or mismatched copy. Its absence counts only
  once the server has applied the plugin's newest publish, so an enable, reload
  or upload never takes the servers still loading it out of rotation. If every
  server lacks something another has, the fleet is misconfigured and the System
  page says so; the gateway keeps one server ready, the one serving the most
  plugins (then the lowest server id). It answers 503 `"not_ready"` otherwise,
  makes no database or Redis call, and its body never names a plugin.
- `/healthz`, the deep check for monitoring: it also reaches Postgres, Redis and
  blob storage (below). It is never a gateway probe: the servers share those, so
  one blip would fail it on every server at once.

These share the runtime that serves API requests and judging, so a server
saturated by a submission burst can answer a probe slowly and fail it even
though it is still working. The gateway probes `/readyz` every 5 s with a 2 s
timeout and ejects a server after 2 misses (`health_interval`, `health_timeout`
and `health_fails` in `examples/Caddyfile.gateway`, and in
`airgap/caddy/Caddyfile.airgap` for an airgap install). An ejected server's
browsers are pinned to a ready one on their next request, and come back to it
only if that one goes away. The admin System page lists each server, whether it
is ready, and the plugins it lacks while another server serves them; the admin
plugin page shows each server's load state and error, and the server's logs have
the rest. Fix the load and click Reload.

A request on a server that lacks a plugin answers `503 PLUGIN_NOT_LOADED_HERE`
(`Retry-After: 10`, naming the plugin in `details.plugin_id`; a result the
plugin governs reads as withheld, `not_loaded_here`) only while a ready server
has it, so the retry reaches that server once the gateway has moved the browser;
otherwise it answers `PLUGIN_NOT_LOADED`, naming the plugin.

A server is live while it touched its heartbeat in the last 15 s (it touches it
every 5 s), about as long as the gateway takes to eject a failing server, so a
server that died, or stopped without leaving cleanly, stops counting within that
window. Every server on one database belongs behind the same gateway: a server
outside it still counts, and can keep the gateway's servers not ready for a
plugin only it serves while the gateway cannot send it a browser. If servers are
ejected under load while their logs show them still serving, raise
`health_timeout` or `health_fails` there and restart the gateway. A Prometheus
scrape of `/metrics` can likewise time out under load; give it a scrape timeout
of a few seconds.

`/healthz` answers 200 with `"status": "ok"` while no dependency is down, and
503 with `"status": "degraded"` and the component that is (`"db"`, `"mq"` or
`"storage"` reads `"down"`) otherwise. `"db": "busy"` means every connection of
this server's pool is in use: the API answers requests that find no connection
with the retryable `DATABASE_BUSY` (503 with `Retry-After`) and serves the rest
as connections come free, so the check stays 200 and an alert on `"down"` does
not page for it. A server that stays busy needs a larger pool, which comes from
Postgres `max_connections` and the CPU count (see Database connections below),
or more servers; a database that is down needs Postgres looked at.
`"plugins": "degraded"` means a plugin enabled for the fleet is not loaded on
this server; it does not change the status code.

`/metrics` and the API documentation (`/swagger-ui`, `/scalar`,
`/api-docs/openapi.json`) answer without authentication, so they are for the
internal network only: every shipped gateway Caddyfile answers them with 404,
and Prometheus scrapes each server's port directly. Keep that port off the
contestants' network.

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

### Command lines

Both binaries refuse any argument they do not know, positional ones included,
and exit with status 2 and a usage message. `broccoli-worker` takes only
`--healthcheck`, `--node-id`, `--help` and `--version`; with none it runs the
worker, which reads every setting from its config file and environment.
`broccoli-server` also takes `serve`, `bundle`, `--openapi` and
`--plugin-group-role`. A compose `command:` override that passes anything else,
such as a config path or a word after the binary's name, stops the container at
start instead of being ignored, so set settings through the role's env file.

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

### Removing a plugin

An uploaded plugin is removed through its upload: open the plugin's details on
the admin Plugins page and select **Remove upload** (the API is
`DELETE /api/v1/admin/plugins/{id}/upload?expect=removed`, or
`expect=bundle_restored` for a plugin a server also bundles). A plugin with no
bundled copy is then removed from every server; one with a bundled copy runs the
bundled version again. A bundled plugin has no upload to remove: select
**Disable** to switch it off on every server.

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

Upgrade all server nodes together, so every server runs the same plugin
lifecycle.

## Password Reset

Use the admin UI when another admin account is available.

The bootstrap admin (`BROCCOLI_BOOTSTRAP_ADMIN_USERNAME`) is created on the
first boot. After that, a restart leaves it alone: a password changed in the UI,
an `admin` role revoked on purpose, or the account renamed or deleted, survives
restarts. A boot creates the account (if no active user has that username) or
resets it to the configured password, and restores the `admin` role and its
built-in permissions, only in two cases:

- no active user holds `system:admin`; this happens automatically;
- `BROCCOLI__BOOTSTRAP__RESET_ADMIN=true` is set; use this when admin accounts
  still exist but nobody can sign in to one.

To recover, edit the server node `.env.server` and restart the server:

```bash
BROCCOLI_BOOTSTRAP_ADMIN_USERNAME=admin
BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD=replace-this-password
BROCCOLI__BOOTSTRAP__RESET_ADMIN=true
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
```

The reset also signs the account out everywhere. After signing in, remove
`BROCCOLI__BOOTSTRAP__RESET_ADMIN` (otherwise every restart resets the password
again) and rotate the bootstrap password.

## Built-in Role Permissions

Each built-in role permission (for example `submission:submit` on `contestant`)
is granted once, on the first boot that knows about it. Revoking one through the
API or UI persists across restarts. A permission that a later release adds to a
built-in role is granted once on the first boot of that release. The recovery
above re-grants the `admin` role's built-in permissions.

## Moving Problems and Contests Between Servers

`broccoli-server bundle export` writes a server's problems, or a whole contest,
as one `.tar.gz`, and `broccoli-server bundle import` restores it onto another
server. Both run with the server's own configuration, so they reach the database
and blob storage exactly as the server does. Progress and the summary go to
stderr; the archive goes to stdout or comes from stdin unless a file is named,
so the commands run through `docker compose exec -T` with no shared directory:

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml \
  exec -T server broccoli-server bundle export --contest 3 > contest-3.tar.gz
docker compose --env-file .env.server -f docker-compose.server.yaml \
  exec -T server broccoli-server bundle import < contest-3.tar.gz
```

Export takes one selection:

| Flag                                | The bundle carries                                                                                                              |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| none                                | Every problem that is not deleted                                                                                               |
| `--problems 1,2,5`                  | Those problems                                                                                                                  |
| `--contest <id>`                    | The contest, its problems, its roster, and the roster's active accounts with their roles                                        |
| `--with-secrets` (with `--contest`) | The accounts' password hashes too. Without it the archive holds no credentials. Secret plugin config fields stay out either way |
| `--out <file>`                      | Writes the archive to the file instead of stdout                                                                                |

Import restores every row in one transaction, committed after every blob is
stored, so a failed import changes nothing. It refuses:

- a bundle from a schema this build does not know: import it with a build at
  least as new as the source;
- a database this build has not migrated yet: start this build's server once
  first;
- a database that already has problems, unless `--replace-problems`, which
  removes every problem the database has (the import summary names, by id and
  title, those the bundle does not carry); and, with `--replace-problems`,
  problems that submissions, code runs or contests still reference, unless
  `--wipe-dependents` deletes those too;
- a contest bundle onto a database that has a contest with the bundle's contest
  id, unless `--replace-contest`. The bundle's contest replaces that one: its
  roster, problem links and contest configs go, and so do its clarifications,
  submissions and code runs, which needs `--wipe-dependents` too. A contest is
  never merged with another one that happens to share its id.

Accounts merge by username, ignoring case:

- An account the target already has, active, stays as it is: its id, password,
  display name and roles are unchanged, whatever the bundle says. It joins the
  imported contest's roster. A bundle never changes what an existing account may
  do.
- An account the target lacks is created with a new id, the bundle's display
  name and the bundle's roles. Without `--with-secrets` it has no password and
  cannot sign in until an admin sets one.
- A role the target lacks is created with the bundle's permissions. A role the
  target has keeps its own permissions.

Plugin configs travel by the rules in the plugin configuration guide's _Move it
with a bundle_ section (`website/docs/building-plugins/configuration.md`,
"Configuration" under "Building plugins" on the documentation site): a bundle
carries its problems' and contest's configs, never a plugin-wide one, and an
import replaces the configs of every problem it replaces. A config field its
plugin marks secret, such as a print station's token, never travels, with or
without `--with-secrets`: the bundle carries an opaque ref in its place.

- An import keeps the target's stored secret only where the target holds the
  bundle's ref for the same item, which means the same stored secret,
  `--replace-problems` included.
- Every other secret is left unset, and the import summary counts them
  (`secrets_unset`): enter them again on the target, such as a contest's print
  station tokens, before the stations can sign in.
- A copy restored from a database dump shares refs with its source only for
  secrets neither side has set again since.
- A problem that replaces another at its id keeps none of that problem's
  secrets.

A secret belongs to its item by key (a print station by its name). A renamed
station, or one deleted and added again under the same name, is a new station:
enter its token again. Two stations cannot share a name.

No read ever shows a secret's value: a set secret reads as an opaque ref,
`{"$secret": "<ref>"}`. When an admin saves a config, the stored secret is kept
only when the save sends that ref back on the same item; the settings form does
this when the field is left empty. A save that leaves a stored secret out, null
or empty, or sends a ref the item does not hold (or one read before the secret
was set again), is refused with `secret_not_stored`: re-enter the secret, or
clear it on purpose. A station's token is required, so it can never be cleared
or left empty (`secret_required`): enter one. Every new value typed for a secret
gets a new ref, even the value already stored. A secret stored before this
release gets its ref when its plugin loads, so saving the settings unchanged
keeps it.

A server reads, saves or deletes a plugin's settings only while it serves that
plugin: it has the plugin loaded at the version the fleet published. Otherwise
every settings route (read, save, delete, the plugin's own settings list, the
config issues) answers a retryable 503, with `Retry-After`, that names the
plugin, and changes or removes nothing:

- `PLUGIN_NOT_LOADED_HERE`: another live server serves it. The gateway moves the
  browser there; retry in a moment.
- `PLUGIN_NOT_LOADED`: no live server loads it.
- `PLUGIN_OUTDATED_HERE`: this server runs another version; it serves once it
  reconciles, within seconds.
- `PLUGIN_DISABLED`: an administrator disabled it.

A problem's, contest's or contest problem's settings dialog shows such a
plugin's saved settings as unavailable, with the reason in place of the form.

The export reads each plugin's schema from its published upload or the server's
plugin directory. The configs of a plugin with neither (removed, or never
installed on the source), and a config whose namespace its plugin no longer
declares, stay out of the bundle, and the export's summary names them. An import
runs without the plugin runtime, so it checks none of the configs it restores.
Until a server checks them, the imported problems count as having config issues:
they land flagged and marked, and a submission judged meanwhile is marked as
judged with config issues. A running server checks the marked problems within a
few seconds (on its next plugin reconcile pass), without a restart, and a server
that is down checks them when it starts. A problem whose plugins no running
server loads stays marked until one does; its config issues page asks to try
again meanwhile.

## Logs

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml logs -f server
docker compose --env-file .env.worker -f docker-compose.worker.yaml logs -f worker
docker compose --env-file .env.infra -f docker-compose.infra.yaml logs -f db redis seaweedfs
```

Designed retryable answers, every answer that carries `Retry-After` and a
`/readyz` that is not ready, log at DEBUG, not ERROR: they say a dependency or a
plugin is not available here yet, which the client retries, not that something
broke. An ERROR line is worth an alert.

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

## Time on an Offline LAN

No node needs NTP. Servers keep wall time by the database clock, so every
replica agrees on when a contest starts and ends, when a token expires and what
the `Date` header says, whatever their own clocks read. Contestant browsers
derive their countdowns from that `Date` header. The queue times its leases on
the Redis clock, and workers compare no wall clocks.

Only log timestamps follow each node's own clock. A server more than a second
off the database logs a warning at boot. Set the infra node's clock once if it
is far off (for example from a phone: `sudo date -s '2026-11-10 09:00:00'`), so
times shown in the UI read right.

## Capacity Planning

Each worker judges one test at a time (`worker.max_concurrency` is 1).
Concurrent runs on one machine share its L2/L3 cache, memory bandwidth and SMT
siblings, and `time_used` (kernel CPU accounting) inflates under that contention
even with CPU pinning, so borderline time limits flip. Scale judging by adding
worker machines. Several worker processes on one machine share those caches too,
so they are no way around it. Total fleet drain time for a contest:

```
drain_seconds = (n_submissions * n_testcases * avg_op_time_seconds)
              / n_worker_machines
```

For 100 submissions × 50 testcases × 0.5 s on 20 machines ≈ 2 minutes.

Each server sizes its plugin execution from its own cores and memory (see the
plugin execution section below); nothing in this formula needs a server setting.

**Database connections**

Pools size themselves. A server wants at most 66 connections: 16 to 40 for
handlers (4 per CPU), up to 24 for plugin host functions (one per plugin CPU
token for plugin SQL, 4 to 16, and one per two tokens for plugin writes, 2 to
8), and 2 of its own (the claim listener and the clock). A plugin call that
finds its pool busy waits for a connection without holding a CPU token, for at
most `plugin.call_timeout_secs`. A worker holds its slots plus 2, so 3 at the
default `max_concurrency` of 1.

At boot a server reads `max_connections`, keeps a tenth of it (at least 10) for
operators and booting nodes, subtracts what the live workers hold, and splits
the rest evenly with the other live servers. Its pools shrink to that share when
they want more, never below 13 connections in all, and the server logs a warning
naming the numbers. A server fits the fleet live when it boots: after adding
nodes against a database with no room to spare, restart the servers so they fit
again.

The infra template's `max_connections=500` fits 5 servers and 30 workers at full
size:

```
500 - 3 superuser slots - 49 reserve = 448 ≥ 5 servers × 66 + 30 workers × 3 = 420
```

Raise `POSTGRES_MAX_CONNECTIONS` in `.env.infra` for a larger fleet. The native
installer sets 500 too, and the development `docker-compose.yaml` 200.

**Behind PgBouncer**

The shipped installs talk to Postgres directly. A PgBouncer in front of it needs
four things, all checked by `release/test/pgbouncer_boot_test.sh` against
PgBouncer 1.26:

1. **Session pooling** (`pool_mode = session`). The boot lock is a session
   advisory lock, the claim loop listens on a channel and the driver prepares
   its statements, none of which survive transaction pooling.
2. **No startup options from the server.** Set
   `database.statement_timeout_secs = 0` and `database.tcp_keepalives = false`
   (see the configuration reference): every session the server opens, its pools,
   the boot lock's and the bundle tool's, then sends none. PgBouncer refuses a
   connection that sends one it does not know ("unsupported startup parameter in
   options").
3. **`ignore_startup_parameters = extra_float_digits`**, which the driver sends
   on every connection. The `edoburu/pgbouncer` image sets this by default; a
   hand-written `pgbouncer.ini` lists it.
4. **A password lookup that covers the plugin roles.** The server gives each
   plugin that uses SQL its own database role, with a password it generates, and
   that role logs in through PgBouncer too, so no `auth_file` can list it.
   PgBouncer looks it up with `auth_query`, run as its `auth_user`, the app
   role; its default query reads `pg_authid`, which only a superuser may, so the
   plugin fails to load with "bouncer config error". Run `user-lookup.sql`
   (`release/pgbouncer/` in the repository, `pgbouncer/` in the airgap bundle)
   once, as the superuser, in Broccoli's database:

   ```bash
   psql -v ON_ERROR_STOP=1 -v app=broccoli -U postgres -d broccoli \
     -f pgbouncer/user-lookup.sql
   ```

   It creates `pgbouncer.user_lookup`, which only the app role may call and
   which answers only for login roles that are not superusers. Then set:

   ```ini
   auth_type = scram-sha-256
   auth_user = broccoli
   auth_query = SELECT uname, phash FROM pgbouncer.user_lookup($1)
   ```

In session mode each client connection holds a server connection, so give
PgBouncer room for the whole fleet: `default_pool_size` at least the most
connections one role opens (the app role's share across every server, see
Database connections above) and `max_client_conn` at least the fleet's total.

A plugin statement that runs past its call's deadline is cut. Talking to
Postgres directly, the server cuts it by closing its connection, which Postgres
notices within 100 ms (`client_connection_check_interval`, which plugin SQL may
not change). That needs a startup option, so behind PgBouncer, with none sent,
the server cuts a statement by cancelling its backend instead
(`pg_cancel_backend`, by the pid it read when it checked the connection out). It
closes only when it sends startup options and a probe connection reads the check
interval back as set, so a pooler that ignores the options is cut by cancelling
too.

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
writes. Each server checks the operations it waits for against its ledger in
Redis every 10 seconds, so an operation lost in that second fails within about
10 seconds of Redis coming back, and its submission goes back to the queue
without spending one of its system-error retries; so does a submission whose
operation could not be published while Redis was down. The same holds for a
Redis restored from an older snapshot. While a server cannot reach Redis it
claims no new submissions; they stay queued for a server that can. The failed
sign-in quota (`auth.login_failure_limit`) and the registration quota are
counted in Redis, so while it is down they admit every request: they guard
against scripted abuse, and an outage must not lock a venue out of signing in.
The bound on device codes is counted in the database and still applies. A worker
that crashes loses nothing: the tasks it held are redelivered to another worker
30 to 35 seconds after it stopped refreshing them.

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

Leave them unset unless a measurement says otherwise; an override goes in the
role's env file. A machine shared with Postgres, Redis or a worker without a
container memory limit should get an explicit `MEMORY_BUDGET` (for example
`4GiB`).

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

A key no binary reads (a removed one such as `POOL_MAX_INSTANCES`,
`EVALUATOR_PARALLELISM` or `SERVER__MAX_BLOCKING_THREADS`, or a typo) still set
in the server's config file or its process environment is ignored and named in
one warning at boot. The whole role env file reaches the container, so an old
value left in `.env.server` shows up in that warning: delete it.

## Multi-Replica Server Identity

Every server replica and every worker needs an id of its own. Left unset,
`BROCCOLI__SERVER__ID` and `BROCCOLI__WORKER__ID` are derived by the binary
itself, by one rule, and `broccoli-server --node-id` or
`broccoli-worker --node-id` prints the id it would take:

- The id is `ROLE-HOST-HASH`, for example `worker-ubuntu-3f9c2a1b`: the first
  label of the hostname, then the first 8 hex digits of
  `sha256("broccoli-node-id:" + MACHINE_ID + ":" + MAC)`.
- `MACHINE_ID` is `/etc/machine-id` (or `/var/lib/dbus/machine-id`). Inside a
  container it is read only from `/run/broccoli/host/machine-id`, the host's
  file mounted there, since an image can bake one in. The server and worker
  compose templates mount it.
- A container with a network of its own also has a hostname of its own, which
  changes whenever it is recreated, so there the id is `ROLE-HASH`, without the
  hostname.
- `MAC` is the address of the first physical network card, by interface name.
  Virtual interfaces (bridges, veths) do not count, so disk-cloned or live-boot
  machines that share a machine id and a hostname still get distinct ids, and
  starting Docker leaves the id alone.
- With no machine id, the hostname alone is used; with neither, a random id, and
  the process warns at boot.
- A container on a bridge network sees no physical card, so its id is
  `ROLE-HASH` of the machine id alone: disk clones that share a machine id get
  the same id there. The installers record the id they ask the image for in the
  host's network, where the card counts; an id left blank in a container relies
  on the machine id alone, so give cloned machines distinct machine ids
  (`systemd-machine-id-setup` after clearing `/etc/machine-id`) or set the id.

The installers never derive an id. A container's hostname and network change
whenever it is recreated, so `install.sh server` and `install.sh worker` (and
the air-gapped `install.sh`, for an id `setup.sh` left blank) ask the image
once: they run its binary with `--node-id` in the host's network with the host's
machine id mounted, and record the answer in `.env.server` or `.env.worker`,
where re-runs keep it. The native worker leaves the id unset and derives it at
every boot, which gives the same answer on the same machine. The env examples
leave the id blank, so an env file copied from one derives its id at boot. Set
the id yourself only when two replicas share one machine: then give each replica
a unique stable value.

**Why it matters:** each server reads its own reply stream
(`operation_tasks:reply:<server_id>`) and owns in-flight work under its ID. A
second process with the same ID refuses to start. A replica whose ID changes at
every restart (the random fallback when no stable ID can be derived) leaves its
old reply stream behind until it expires, 10 minutes later.

A running node renews its claim on its id every few seconds. If another live
process holds the id (two nodes were started with one id, or one booted while
Redis was down and its twin was already running), the node loses its renewals
and, after about 15 seconds of losing them, exits at once with status 75. A
supervisor restarts it (the compose templates use `restart: unless-stopped`),
and the restarted node refuses to start while the twin is alive, naming the id.
An exit with status 75 therefore means two nodes share an id: give one of them
its own.

If you bypass the installer, check that no two replicas' env files name the same
id before starting them.

## Recovering In-Flight Submissions After a Server Restart or Crash

Every server re-dispatches its own unfinished work automatically, and the others
take over the work of one that stops:

- **Graceful stop** (`docker compose stop`, a rolling restart, a redeploy): the
  server releases the submissions, rejudges and code runs it was judging as it
  shuts down, and the other servers pick them up within one steal-scan interval
  (`steal_scan_interval_secs`, default 15 s). Nobody has to resubmit.
- **Restart after a crash**: a server that comes back with the same
  `BROCCOLI__SERVER__ID` releases and re-dispatches the work its previous
  instance left behind as it boots. That work no longer counts against its
  in-flight limit.
- **Crash without a restart**: the other servers steal the dead server's work
  once its lease expires (`lease_ttl_secs`, default 60 s). On a single server
  the work waits until it boots again, or until the stuck-job detector fails it
  after `max_inflight_secs` (default 1 hour) so it can be rejudged.

When a crashed server will not come back:

1. Confirm the failed server is dead (not merely network-partitioned). Two
   replicas with the same ID refuse to run side by side, but a partitioned one
   still holds its work.
2. Either start a replacement with the same `BROCCOLI__SERVER__ID` (its boot
   recovery re-dispatches the stranded work), or rejudge the affected
   submissions once the stuck-job detector has failed them.

A dead server's reply stream needs no cleanup: it expires 10 minutes after the
server stopped.
