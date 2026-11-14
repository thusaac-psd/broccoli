# Upgrade Guide

Use pinned image tags. Do not use `latest` for contest deployments.

## How the Database Schema Upgrades

An upgrade keeps every contest, account, problem and submission. The server owns
the database schema as an ordered list of migrations, and the table
`seaql_migrations` records the ones a database has applied. Workers never change
the schema.

When a server starts it takes the boot lock (a Postgres advisory lock), applies
the migrations the database lacks, oldest first, each in its own transaction,
and releases the lock. Servers that start together wait for the lock; the first
applies the migrations and the others find nothing left to apply.

A server refuses to start, and changes nothing, when the database:

- records a migration this build does not know. A newer build migrated it. The
  log names the unknown migration and the newest one this build has.
- holds tables but no migration record, so it is not a Broccoli database.

## Back Up Before Upgrading

Take a database dump right before upgrading a server. A migration has no undo:
the dump is how you return to the previous version (see "Rollback").

```bash
docker compose -p broccoli-infra --env-file .env.infra -f docker-compose.infra.yaml \
  exec -T db sh -c 'pg_dump -U "$POSTGRES_USER" -Fc "$POSTGRES_DB"' > broccoli-$(date -u +%Y%m%dT%H%M%SZ).dump
```

Run it on the infra node, with the Compose project, env file and compose file
that node was installed with:
`-p broccoli-single-host --env-file .env.single-host -f docker-compose.single-host.yaml`
on a single-host install, and
`-p compose --env-file .env.infra -f docker-compose.infra.yaml.template` from
`compose/` in an air-gapped bundle. With `storage.backend = "database"` the dump
holds the uploaded files too. With SeaweedFS they live in its volume; blobs are
named by their content and never change, so a restored database finds every file
it references. For a full backup, also copy the SeaweedFS volume.

## Which Upgrades Can Roll

A server keeps working against a database that a newer build migrated as long as
each new migration is additive: a new table, a new nullable or defaulted column,
a new index. Every other change (renaming or dropping a column, changing its
type, a new `NOT NULL` without a default, a tighter constraint) follows
expand/contract over two releases. The first adds the new shape and keeps
writing the old one; the second, released once no server runs the old version,
drops the old shape. Each step is additive for the servers that run alongside
it, so every release can roll server by server.

An old server that restarts after a new one has migrated the database refuses to
start (it does not know the new migration). Once the first upgraded server is
up, finish the roll instead of restarting old servers.

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

Servers share the `idempotency_key` table, and a server only recognises the key
states its own version writes, so a client retry that lands on a server of
another version can run a request the first one already recorded (for example,
create the same users twice). When the release notes say a release adds
idempotency key states, upgrade every server node in one step outside contest
hours instead of one at a time.

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

During the window, submissions should complete successfully.

## Upgrading from a new bundle offline

To move a node to a newly extracted bundle without a registry:

1. Copy the role's env file (and `connection.env`, plus `server-secrets.env` on
   server nodes) from the old bundle directory into the new one.
2. Run `./install.sh <role>` in the new directory. It loads the bundled images
   (`images/*.tar.gz`), keeps the env file's secrets, takes any changed settings
   from `connection.env` and `server-secrets.env`, and points
   `BROCCOLI_SERVER_IMAGE`, `BROCCOLI_WORKER_IMAGE` (same variant) or
   `CADDY_IMAGE` at the images it just loaded. It leaves infra image tags alone.

To load the images by hand instead, then edit the image tag in the env file:

```bash
for f in images/*.tar.gz; do gzip -dc "$f" | docker load; done
```

## Rollback

Workers, and servers of a release that added no migration, roll back by image:

1. Restore the previous image tag in the affected node's `.env.<role>`.
2. Run
   `docker compose --env-file .env.<role> -f docker-compose.<role>.yaml up -d`.
3. Confirm the role health check.
4. Check logs for failed submissions.

Roll back server nodes one at a time when a gateway is present. Roll back
workers before servers when both roles were upgraded.

Once the new release has applied a migration, the previous servers refuse the
database, so a server rollback restores the dump taken before the upgrade.
Submissions made since that dump are lost.

1. Stop every server and worker.
2. Restore the dump into the database, as the database superuser:

   ```bash
   docker compose -p broccoli-infra --env-file .env.infra -f docker-compose.infra.yaml \
     exec -T db sh -c 'pg_restore -U "$POSTGRES_USER" -d "$POSTGRES_DB" --clean --if-exists' < broccoli-<time>.dump
   ```

3. Restore the previous image tags and start the servers, then the workers.

## Worker Image Variants

Use the `-icpc` worker for normal contests. Use `-base` only when deriving a
custom image with your own toolchains. Use `-full` for open-format contests that
need Node.js, Rust, Go, Pascal, or Kotlin.
