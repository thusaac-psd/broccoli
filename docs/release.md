# Release Operations

Platform releases are cut from git tags matching `vX.Y.Z` or prerelease tags
such as `vX.Y.Z-rc.1`.

## Required GitHub Settings

Create a `release` environment with reviewer approval. Add these environment
secrets:

- `ACR_USERNAME`
- `ACR_PASSWORD`

`GITHUB_TOKEN` is provided by GitHub Actions and is used for GHCR and GitHub
Release publishing.

## Release Checklist

1. Confirm `cargo test --workspace --locked` passes.
2. Confirm `pnpm build` passes.
3. Build and lint Dockerfiles in CI.
4. Push a candidate tag:

```bash
git tag v0.3.0-rc.1
git push origin v0.3.0-rc.1
```

5. Approve the `release` environment before image publishing begins.
6. Rehearse the role topology below before marking the GitHub Release as
   non-prerelease.

## Production Topology

LAN and cloud deployments use the same roles. The network addresses and firewall
rules differ, but the service shape does not:

- Infra machine: PostgreSQL, Redis, and SeaweedFS object storage.
- Server machines: one `server` container per machine.
- Worker machines: one `worker` container per judge host.
- Optional gateway machine: Caddy load balancer across server machines.

Do not use `docker compose --scale worker=N` for production. Each worker needs a
stable unique `BROCCOLI__WORKER__ID`, isolated cgroup state, and its own host
CPU/memory budget.

## Manual Role Rehearsal

Use at least three fresh Linux+Docker VMs for the production rehearsal: one
infra node, one server node, and one worker node. Add a second server plus a
gateway node for the redundancy rehearsal.

For a guided terminal setup, run `./install.sh` without arguments. The installer
will ask for this machine's role and, on worker nodes, which worker image to
use. The scripted examples below are the non-interactive equivalent.

On the infra node:

```bash
tar -xzf broccoli-platform-v0.3.0-rc.1.tar.gz
cd broccoli-platform-v0.3.0-rc.1
BROCCOLI_INFRA_HOST=10.0.0.10 ./install.sh infra
docker compose -f docker-compose.infra.yaml ps
```

Record the generated values from `.env`:

- `BROCCOLI__DATABASE__URL`
- `BROCCOLI__MQ__URL`
- `BROCCOLI__AUTH__JWT_SECRET`
- `BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD`
- `BROCCOLI__STORAGE__OBJECT_STORAGE__*`

On each server node, provide those values and run:

```bash
BROCCOLI__SERVER__ID=server-1 \
BROCCOLI__DATABASE__URL='postgres://postgres:...@10.0.0.10:5432/broccoli' \
BROCCOLI__MQ__URL='redis://:...@10.0.0.10:6379' \
BROCCOLI__AUTH__JWT_SECRET='...' \
BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD='...' \
BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT='http://10.0.0.10:8333' \
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY='...' \
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY='...' \
./install.sh server
curl -fsS http://127.0.0.1:3000/healthz
```

On each worker node, use the same infra and storage values:

```bash
BROCCOLI__WORKER__ID=worker-1 \
BROCCOLI__DATABASE__URL='postgres://postgres:...@10.0.0.10:5432/broccoli' \
BROCCOLI__MQ__URL='redis://:...@10.0.0.10:6379' \
BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT='http://10.0.0.10:8333' \
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY='...' \
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY='...' \
./install.sh worker
docker compose -f docker-compose.worker.yaml ps
```

Node clocks do not need to agree. Servers keep wall time by the database clock:
each one measures its offset to Postgres every 30 to 60 s and serves contest
phases, token expiry, rate limits and the HTTP `Date` header from it. The queue
measures idle times and TTLs on the Redis clock, and workers compare no wall
clocks at all. A server whose own clock is more than a second off logs a warning
at boot, because its log timestamps are off by that much.

Every database connection sends a 60 s `statement_timeout` and server-side TCP
keepalives as startup options. A PgBouncer in front of Postgres rejects those
unless its `ignore_startup_parameters` lists `options`; otherwise set
`BROCCOLI__DATABASE__STATEMENT_TIMEOUT_SECS=0` and
`BROCCOLI__DATABASE__TCP_KEEPALIVES=false` on servers and workers and configure
the timeout on the Postgres role instead.

For server redundancy, add another server node with a different
`BROCCOLI__SERVER__ID`, then install a gateway:

```bash
BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' ./install.sh gateway
curl -fsS http://127.0.0.1/healthz
```

## Single-Host Rehearsal

The single-host role is only a packaging smoke or tiny demo path:

```bash
BROCCOLI_ADMIN_PASSWORD='replace-this-password' ./install.sh single-host
curl -fsS http://127.0.0.1:3000/healthz
```

Do not use this as the production reference architecture.

## Load Rehearsal

After infra, at least one server, and at least one worker are healthy, run a
small judged check (the same one `install.sh` runs on single-host installs),
then bounded load, through the server or gateway URL:

```bash
./stress-test/broccoli-stress-test \
  --url http://10.0.0.21:3000 \
  --admin-username admin \
  --admin-password "$BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD" \
  --total 20 \
  --rate 5 \
  --concurrency 5 \
  --p95-budget-ms 60000

./stress-test/broccoli-stress-test \
  --url http://10.0.0.21:3000 \
  --admin-username admin \
  --admin-password "$BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD" \
  --total 200 \
  --rate 10 \
  --concurrency 30 \
  --p95-budget-ms 20000 \
  --json
```

For a gateway rehearsal, use the gateway URL and restart one server node during
the test window. The request path should stay healthy through Caddy while the
remaining server receives traffic.

## Evidence To Keep

- `docker compose -f docker-compose.infra.yaml ps` from the infra node.
- `docker compose -f docker-compose.server.yaml ps` from each server node.
- `docker compose -f docker-compose.worker.yaml ps` from each worker node.
- `/healthz` result from every server and the gateway if present.
- Stress-test JSON output with VM sizes, server count, and worker count.
