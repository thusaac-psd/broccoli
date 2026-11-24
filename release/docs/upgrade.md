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
a new index. A release whose migrations are all additive rolls server by server
(see "Standard Upgrade" and "Rolling-Upgrade Rehearsal").

Any other change (renaming or dropping a column, changing its type, a new
`NOT NULL` without a default, a tighter constraint, a grant taken away) breaks
the servers still running the previous release. A release with such a migration
is upgraded with every server stopped: stop all of them, then start the first
server of the new release, which applies the migrations, then the rest. Workers
can keep running. The release notes say which kind a release is.

This release is one of them: it tightens a constraint on refresh tokens, drops a
column of device-code approvals, and takes plugin SQL's grant on failure
diagnostics away. Stop every server before the first one of this release starts.

An old server that restarts after a new one has migrated the database refuses to
start (it does not know the new migration). Once the first upgraded server is
up, finish the upgrade instead of restarting old servers.

## Standard Upgrade

Infra images are upgraded only during a maintenance window. For normal platform
releases, upgrade application roles first:

1. Upgrade each worker node by editing `BROCCOLI_WORKER_IMAGE` in that node's
   `.env.worker`, then:

```bash
docker compose --env-file .env.worker -f docker-compose.worker.yaml up -d --no-deps worker
docker compose --env-file .env.worker -f docker-compose.worker.yaml ps worker
```

2. Upgrade server nodes one at a time by editing `BROCCOLI_SERVER_IMAGE` (for a
   release whose migrations are not all additive, stop every server node first;
   see "Which Upgrades Can Roll"), then:

```bash
docker compose --env-file .env.server -f docker-compose.server.yaml up -d --no-deps server
docker compose --env-file .env.server -f docker-compose.server.yaml ps server
set -a; . ./.env.server; set +a
curl -fsS http://127.0.0.1:${BROCCOLI_HTTP_BIND##*:}/healthz
```

3. If a gateway is present, keep it running while server nodes roll. Restart it
   only after all upstream server health checks pass.

### One frontend version per browser

Each server serves the web app it was built with: an `index.html` that is never
cached and content-hashed chunks under `/assets/` that are cached for good.
While servers of two releases run behind one gateway, a browser must not load
the page from one and its chunks from the other. The shipped gateways
(`examples/Caddyfile.gateway`, and `airgap/caddy/Caddyfile.airgap`) see to it:
they pin each browser to one server with the `broccoli_upstream` cookie, so it
keeps loading the page and every chunk from the same build. A browser without
the cookie, or whose server is down, is sent to the next server round robin and
pinned to it. The cookie names its server by a hash of the server's address and
nothing else, so a gateway that restarts, or a second gateway in front of the
same servers, keeps every browser on the server it had.

The command-line clients are not pinned, and need not be: they read only the
API, which both releases of a roll answer, and never load the web app's chunks.

A browser switches builds only when its own server stops for the upgrade. Its
open page is still the old build; the next chunk it loads comes from a new
server, which does not have the old chunk, and the page reloads itself once and
continues on the new build. That one automatic reload is all a contestant sees.
A release upgraded with every server stopped (see "Which Upgrades Can Roll")
never mixes builds: the first page after the restart is the new one.

A gateway of your own that sends each request to any server can mix builds
during a roll. Give it the same stickiness by cookie (not by client address: a
venue shows one address for all its contestants), or upgrade every server
together: stop them all, then start them.

Servers share the `idempotency_key` table, and a server only recognises the key
states its own version writes, so a client retry that lands on a server of
another version can run a request the first one already recorded (for example,
create the same users twice). When the release notes say a release adds
idempotency key states, upgrade every server node in one step outside contest
hours instead of one at a time.

## Rolling-Upgrade Rehearsal

Rehearse a rolling upgrade, for a release whose migrations are all additive, on
the same role topology as production: infra node, server node or nodes, and one
worker per judge host.

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
