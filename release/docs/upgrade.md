# Upgrade Guide

Use pinned image tags. Do not use `latest` for contest deployments.

## Standard Upgrade

Infra images are upgraded only during a maintenance window. For normal platform
releases, upgrade application roles first:

1. Upgrade each worker node by editing `BROCCOLI_WORKER_IMAGE` in that node's
   `.env.worker`, then:

```bash
docker compose --env-file .env.worker -f docker-compose.worker.yaml up -d --no-deps worker
docker compose --env-file .env.worker -f docker-compose.worker.yaml ps worker
```

2. Upgrade server nodes one at a time by editing `BROCCOLI_SERVER_IMAGE`, then:

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
docker compose --env-file .env.server -f docker-compose.server.yaml ps server
set -a; . ./.env.server; set +a
curl -fsS http://127.0.0.1:${BROCCOLI_HTTP_BIND##*:}/healthz
```

3. If a gateway is present, keep it running while server nodes roll. Restart it
   only after all upstream server health checks pass.

The workers-first order preserves the rolling-upgrade path while per-replica
operation-result queues are introduced. The legacy `operation_results` queue is
kept for one release for compatibility and is scheduled for removal in the next
release.

### Upgrading to the release with SHA-256 refresh tokens

This release stores refresh-token validators as SHA-256 digests instead of
argon2 hashes. A new server still accepts the old format, so sessions issued
before the upgrade keep working. An older server cannot read the new format
and answers a refresh with 500. Upgrade every server node together (or drain
the old ones from the gateway first) rather than mixing versions for long; a
contestant whose refresh reaches an old node during the overlap is retried by
the web client but can be signed out.

## Rolling-Upgrade Rehearsal

Rehearse the upgrade on the same role topology as production: infra node, server
node or nodes, and one worker per judge host.

```bash
export OLD_VERSION=v0.2.0
export NEW_VERSION=v0.3.0-rc.1
```

Install the old version first, then update worker nodes:

```bash
sed -i.bak \
  "s#^BROCCOLI_WORKER_IMAGE=.*#BROCCOLI_WORKER_IMAGE=ghcr.io/thusaac-psd/broccoli/broccoli-worker:${NEW_VERSION}-icpc#" \
  .env.worker
docker compose --env-file .env.worker -f docker-compose.worker.yaml up -d --no-deps worker
docker compose --env-file .env.worker -f docker-compose.worker.yaml ps worker
```

Run correctness through a server or gateway URL. Then update server nodes one at
a time:

```bash
sed -i.bak \
  "s#^BROCCOLI_SERVER_IMAGE=.*#BROCCOLI_SERVER_IMAGE=ghcr.io/thusaac-psd/broccoli/broccoli-server:${NEW_VERSION}#" \
  .env.server
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
set -a; . ./.env.server; set +a
curl -fsS http://127.0.0.1:${BROCCOLI_HTTP_BIND##*:}/healthz
```

During the window, submissions should complete successfully and server logs
should not show legacy `operation_results` warnings for new work.

## Node IDs and heartbeats

From this release every server and worker claims its ID in Redis at boot and
keeps a heartbeat key alive, and a second live process with the same ID
refuses to start (or stops within about 15 seconds). When the ID is unset,
each node derives one for its host instead of `hostname -s` (servers) or
`worker-1` (workers). Set `BROCCOLI__SERVER__ID` and `BROCCOLI__WORKER__ID`
explicitly if anything outside Broccoli depends on the old values. An explicit
but invalid ID now fails boot instead of being replaced.

Finish upgrading every server node within an hour of the first one. With
`DISPATCHER_LEASE_STEAL_ENABLED=true`, the reply-queue sweeper treats a server
without a heartbeat as dead, and servers from earlier releases never write one.
After the sweeper's one-hour grace it would delete a still-running old
server's result queue.

## Contests without a contest type

Every contest now needs a contest type. Contests created by earlier releases
without one accept no submissions until an admin sets it, where they used to
be judged by whichever type sorted first. Before a contest, list them on the
infra node and set a type for each in the contest editor:

```bash
set -a; . ./.env.infra; set +a
docker compose --env-file .env.infra -f docker-compose.infra.yaml exec db \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
  -c "SELECT id, title FROM contest WHERE contest_type IS NULL AND deleted_at IS NULL"
```

## Duplicate rows blocking a unique index

Servers now refuse to start when a boot-time unique index cannot be built
because duplicate rows already exist. Earlier releases only logged a warning, so
a database can carry duplicates from then: most often a second bootstrap
`admin` from two servers' simultaneous first boot, or repeated attachment or
additional-file paths. The error names the index and prints a `SELECT` that
lists the duplicate keys; run it on the infra node:

```bash
set -a; . ./.env.infra; set +a
docker compose --env-file .env.infra -f docker-compose.infra.yaml exec db \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c '<query from the error>'
```

Then keep one row per key. For duplicate usernames, soft-delete the extra
accounts (the index only covers accounts that are not deleted):

```sql
UPDATE "user" SET deleted_at = now()
WHERE username = 'admin' AND deleted_at IS NULL
  AND id <> (SELECT min(id) FROM "user" WHERE username = 'admin' AND deleted_at IS NULL);
```

For attachment or additional-file paths, delete the older copies in the problem
editor or with `DELETE ... WHERE id = ...`. Restart the server afterwards.

If you cannot clean up before the contest, set
`BROCCOLI__DATABASE__ALLOW_MISSING_UNIQUE_INDEXES=true` on the servers to start
anyway; the error is still logged and the invariant stays unenforced. This
switch exists for one release only.

## Compose project names

Env files written by this release set `COMPOSE_PROJECT_NAME=broccoli-<role>`,
so each role is its own Compose project. Env files from earlier releases have
no project name, and Compose uses the bundle directory's name instead, for
example `broccoli-platform-v020` for `broccoli-platform-v0.2.0/`. A role's
volumes, including the database, are named after its project, so the same env
file in a new release's directory would start on new, empty volumes.

Before moving such an env file, pin the project it runs as today. On the node,
in the old bundle directory:

```bash
docker compose ls          # the NAME column is the project
docker volume ls --filter label=com.docker.compose.volume=postgres_data \
  --format '{{.Label "com.docker.compose.project"}}'
echo "COMPOSE_PROJECT_NAME='broccoli-platform-v020'" >> .env.infra
```

Never rename the project of a role that is already running. `install.sh`
refuses to start a role whose env file has no project name while that role's
data volume exists only under a different project, and prints the name to pin.

## Upgrading from a new bundle offline

To move a node to a newly extracted bundle without a registry:

1. Pin the project name as above if the env file has none.
2. Copy the role's env file (and `connection.env`, plus `server-secrets.env` on
   server nodes) from the old bundle directory into the new one.
3. Run `./install.sh <role>` in the new directory. It loads the bundled images
   (`images/*.tar.gz`), keeps the env file's secrets, takes any changed
   settings from `connection.env` and `server-secrets.env`, and points
   `BROCCOLI_SERVER_IMAGE`, `BROCCOLI_WORKER_IMAGE` (same variant) or
   `CADDY_IMAGE` at the images it just loaded. It leaves infra image tags
   alone.

To load the images by hand instead, then edit the image tag in the env file:

```bash
for f in images/*.tar.gz; do gzip -dc "$f" | docker load; done
```

## Rollback

1. Restore the previous image tag in the affected node's `.env.<role>`.
2. Run
   `docker compose --env-file .env.<role> -f docker-compose.<role>.yaml up -d`.
3. Confirm the role health check.
4. Check logs for failed submissions or operation-result queue warnings.

Roll back server nodes one at a time when a gateway is present. Roll back
workers before servers when both roles were upgraded.

Rolling the server back past the release with SHA-256 refresh tokens signs
everyone out: the older server cannot read the new refresh tokens and answers
every refresh with 500. After rolling back, delete those tokens so clients get
a clean 401 and go to the login page instead:

```sql
DELETE FROM refresh_tokens WHERE validator LIKE 'sha256$%';
```

## Worker Image Variants

Use the `-icpc` worker for normal contests. Use `-base` only when deriving a
custom image with your own toolchains. Use `-full` for open-format contests that
need Node.js, Rust, Go, Pascal, or Kotlin.
