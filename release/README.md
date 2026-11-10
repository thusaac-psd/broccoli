# Broccoli Platform Bundle

This bundle contains the Broccoli server image, base/ICPC/full worker images,
PostgreSQL, Redis, optional SeaweedFS object storage, Caddy, default plugins,
the role-aware installer, and the stress-test smoke binary.

## Deployment Shape

Use the same roles for LAN and cloud deployments:

- `infra`: one machine for PostgreSQL, Redis, and optional SeaweedFS.
- `server`: one Broccoli server process per server machine.
- `worker`: one judge worker per worker machine.
- `gateway`: optional Caddy load balancer in front of multiple servers.

The `single-host` role exists only for release rehearsal or small demos.

## Install

Install infra first:

```bash
./install.sh infra
```

When run from a terminal without a role argument, the installer presents a
guided role menu. Infra, server, worker, and single-host installs also present a
storage menu:

- `object_storage` (recommended for any contest): use bundled SeaweedFS
  S3-compatible storage. Keeps testcase blob traffic off the Postgres connection
  pool, which is what saturates first under contest concurrency.
- `database`: store uploads/results in PostgreSQL. Only for tiny demos with no
  S3 available. Every blob fetch holds a DB connection for the entire stream, so
  this backend cannot survive even a 50-submission burst.

Worker and single-host installs also present a worker image menu:

- `icpc`: C, C++, Java, Python; recommended for normal contests.
- `full`: ICPC plus Node.js, Go, Rust, Pascal, Kotlin.
- `base`: isolate sandbox only; intended for custom derived images.
- `custom`: an operator-provided image tag.

Infra writes `.env.infra`, worker-safe `connection.env`, and server-only
`server-secrets.env`. Copy `connection.env` into each server and worker bundle
directory. Copy `server-secrets.env` only into server bundle directories, then
run:

```bash
./install.sh server
./install.sh worker
```

The server installer imports both files. The worker installer imports only
`connection.env`, asks only for node-specific choices like worker ID and worker
image, then shows a review screen. Choose `e` at the review prompt to edit the
generated `.env.<role>` before any containers start.

Before starting containers, the installer also checks the machine clock with the
available platform time service (`timedatectl`, `chrony`, or macOS network time)
and compares against an HTTPS `Date` header when `curl` and `python3` are
available. Set `BROCCOLI_SKIP_TIME_CHECK=1` only for offline rehearsals.

To install multiple roles from the same extracted bundle directory on one host,
pass them in order:

```bash
./install.sh infra server
```

Each role runs as its own Compose project (`COMPOSE_PROJECT_NAME=broccoli-<role>`
in its env file), so roles on one host get separate networks, just as they
would on separate machines. A role's volumes, including the database, are
named after its project. Env files written by earlier releases have no project
name, so Compose names the project after the bundle directory. Before you copy
such a file into a new bundle directory, pin the old name; see
[docs/upgrade.md](docs/upgrade.md#compose-project-names). The installer refuses
to start a role whose data volume exists only under another project.

## Optional plugins

Plugins for particular events, such as the Codelink contest formats, ship in
`plugins-available/` and load only once enabled. Enable them at install time:

```bash
BROCCOLI_PLUGINS="codelink-qualifier codelink-bracket" ./install.sh server
```

or on a running install:

```bash
./enable-plugin.sh codelink-qualifier codelink-bracket
```

Run `./enable-plugin.sh` with no arguments to list the optional plugins. Enable
a plugin on every server and worker node, then restart every server. **Reload
All** on the admin Plugins page reloads only the server that handled the
click, so with several servers the others keep running without the plugin.

Uploading a plugin archive from the admin Plugins page does not work with these
templates, by design: servers and workers mount `./plugins` read-only. An
upload would reach only the one server that handled it, never the workers or
the other servers, and a read-only mount means a stolen admin session cannot
install code on contest machines. Ship plugins in the bundle's `plugins/` or
`plugins-available/` directory on every node instead.

For server redundancy, install a gateway node with:

```bash
BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' ./install.sh gateway
```

Each server must then trust the gateway's address for `X-Forwarded-For`;
otherwise every contestant's login counts against the gateway's single rate
limit. The gateway install prints the command to run on each server, for
example:

```bash
BROCCOLI_GATEWAY_HOSTS='10.0.0.5' ./install.sh server
```

This sets `BROCCOLI__SERVER__TRUSTED_PROXIES` to exactly those addresses, also
on a server that is already installed.

## Files

- `docker-compose.infra.yaml.template`: PostgreSQL, Redis, optional SeaweedFS.
- `docker-compose.server.yaml.template`: one HTTP/API server.
- `docker-compose.worker.yaml.template`: one judge worker.
- `docker-compose.gateway.yaml.template`: optional Caddy load balancer.
- `.env.*.example`: role-specific configuration examples.
- `connection.env`: worker-safe infra settings generated by infra/single-host
  installs.
- `server-secrets.env`: server-only JWT/bootstrap settings generated by
  infra/single-host installs.
- `plugins/`: plugins loaded by default.
- `plugins-available/`: optional plugins, loaded once enabled with
  `enable-plugin.sh` or `BROCCOLI_PLUGINS`.
- `docs/`: upgrade, TLS, and operations guides.
- `examples/`: Caddy configs and custom worker image example.
