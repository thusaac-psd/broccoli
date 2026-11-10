# Air-Gap LAN Deployment Runbook

This runbook walks an operator through assembling a broccoli deployment bundle
on a networked staging box and installing it on a zero-network LAN (a live-boot
contest venue, for example). Follow the sections in order: the server must be
installed and reachable before workers and contestant machines trust its
certificate.

## 1. Overview

A normal broccoli deployment leans on the internet in three places: it pulls
container images from a registry, it fetches TLS certificates via ACME (Let's
Encrypt), and it resolves DNS for the public hostname. None of that is available
at an air-gapped venue. The `release/airgap/` tooling replaces all three with a
**single self-contained bundle** built ahead of time on a machine that _does_
have internet access, then carried to the venue on removable media (USB drive,
external SSD) and installed with zero network calls.

The bundle covers three roles:

- **server** — the broccoli API/dispatcher plus its infra (Postgres, Redis,
  SeaweedFS/S3), fronted by Caddy serving an internal-CA TLS leaf.
- **worker** — the judging box that runs the isolate sandbox and executes
  submissions.
- **contestant** — a contestant's machine, which only needs the LAN root CA
  trusted (so HTTPS to the server doesn't warn) and the `broccoli` CLI on its
  `PATH`.

Every role installs from the same bundle tree. The recommended path is the
guided one-click installer, `setup.sh --role {server|worker|contestant}`, which
wraps the underlying per-role engine `install.sh` with runtime detection, a
go/no-go preflight, and auto-generated configuration (§4). `install.sh` remains
available directly for operators who manage their own `.env` files. Nothing
under `release/airgap/` ever shells out to `curl`, `wget`, `apt`, `pip`, or
`docker pull` — that invariant is enforced by
`release/airgap/test/offline_guard_test.sh` and `install_test.sh`, and it's what
makes the target-side scripts safe to run with no network at all.

## 2. On the staging box (networked)

The staging box is any machine with internet access, Docker, and this repo
checked out. It assembles the bundle:

```bash
release/airgap/build-bundle.sh --version <V> [--lan-host <host-or-ip>] --tar
```

- `--version <V>` names the bundle (`[A-Za-z0-9._-]` only); the tree lands at
  `./dist/broccoli-airgap-<V>/` by default (override with `--output`).
- `--lan-host <host-or-ip>` is optional at assembly time. If you already know
  the venue's LAN IP or hostname, pass it here and `build-bundle.sh` pre-issues
  the server's TLS leaf during assembly (see step 8, `root.key` security) and
  bakes the address into the cluster-secret sidecar's `cluster-secrets.env` as
  `BROCCOLI_SERVER_HOST`, so worker installs (see "Worker host (one-click)"
  below) are fully one-click — `--lan-host` becomes omittable on the worker.
- `--tar` additionally produces `broccoli-airgap-<V>.tar.zst` next to the tree,
  convenient for copying to a single file on USB.

`build-bundle.sh` produces **three** outputs side by side under `--output`
(default `./dist/`): the client bundle tree `broccoli-airgap-<V>/`, which is
carried to **all** roles (server, workers, contestants); a server-only sidecar
`broccoli-airgap-<V>.server-secret/` (mode `0700`) that holds the CA and leaf
**private keys** and is delivered **only** to the server host — never to workers
or contestants; and a cluster-secret sidecar
`broccoli-airgap-<V>.cluster-secret/` (mode `0700`) holding a single file,
`cluster-secrets.env`, with the shared Postgres/Redis/S3 passwords the server
and every worker must agree on, plus an optional `BROCCOLI_SERVER_HOST` — this
one is delivered to the server host **and** every worker host, but **never** to
contestants. Unlike the bundle tree, the cluster-secret sidecar is
**unmanifested**: it is not listed in `manifest.sha256` and is not covered by
the integrity check in step 3. It runs `ca/mint-ca.sh` to mint a fresh internal
root CA into the server-secret sidecar (`root.key` lives there, `chmod 0600`),
copies only the public `ca/root.crt` into the bundle tree, then stages
everything an install needs:

- `images/` — `docker save` tarballs for the server, worker, Postgres, Redis,
  SeaweedFS, and Caddy images (skipped when assembling structurally with
  `--skip-images`, which the bundle's own CI test uses). **Before transferring a
  bundle, confirm `images/` actually contains all six tarballs** — a bundle
  assembled with `--skip-images`, or assembled before image build/save is wired
  up, will not have them, and `load-bundle.sh` will fail with
  `no images/*.tar in bundle` on the target when it tries to `docker load` them.
- `compose/` — the server, infra, server-behind-gateway override
  (`docker-compose.server-airgap.yaml.template`), and Caddy TLS gateway
  (`docker-compose.gateway-airgap.yaml.template`) compose templates, plus
  `compose/.env.server.example` and `compose/.env.infra.example` (real env files
  are _not_ shipped — see the server install section below).
- `compose/plugins/` — the default contest-format and evaluator/checker plugins
  (built `.wasm` + frontend assets), copied out of the server image at assembly.
  The server and worker compose files bind-mount `./plugins:/plugins:ro`, which
  overlays the image-baked copy, so this directory must be present or the judge
  boots with an empty plugin registry and cannot evaluate submissions. It is part
  of the manifested tree, so `load-bundle.sh` integrity-verifies the plugin code
  along with everything else. (A `--skip-images` structural bundle omits it, as it
  omits the images themselves.)
- `compose/plugins-available/` — optional plugins for particular events, such as
  the Codelink contest formats. They are not loaded until enabled, either with
  `BROCCOLI_PLUGINS="codelink-qualifier codelink-bracket"` on the server
  `install.sh` run, or later with `./enable-plugin.sh codelink-qualifier
  codelink-bracket` followed by **Reload All** on the admin Plugins page.
  `./enable-plugin.sh` with no arguments lists them. An enabled plugin stays
  covered by the integrity check: it must match its copy here byte for byte.
- `cli/` — the musl-static `broccoli` contestant CLI binary.
- `ca/` — `root.crt` (public, ships everywhere) and `issue-leaf.sh`; NO private
  key lives here. The CA/leaf private keys live only in the
  `broccoli-airgap-<V>.server-secret/` sidecar.
- `caddy/Caddyfile.airgap` — the explicit-TLS Caddy site, mounted un-rendered
  into the gateway container (Caddy expands its variables from the container
  environment at load time).
- `trust-ca/` — the per-OS root-CA trust helpers (`linux.sh`, `macos.sh`,
  `windows.ps1`).
- `setup.sh`, `install.sh`, `load-bundle.sh` — the target-side scripts.
  `setup.sh` is the recommended guided one-click installer (§4); `install.sh` is
  the underlying per-role engine it wraps.
- `lib/` — the installer's shared shell libraries (`runtime.sh`, `answers.sh`,
  `envgen.sh`, `preflight.sh`, `manifest.sh`), sourced by `setup.sh` and
  `install.sh`.
- `bundle.json` — provenance (version, source git SHA, build time, role list).
- `manifest.sha256` — a `sha256sum` of every other file in the tree, sorted by
  relative path, used to detect transfer corruption or tampering.

The frontend is baked fresh into the server image as part of this staging build
(not copied by mtime from a stale artifact — see the
`e2e-frontend-deploy-staleness` lesson), and should be verified behaviorally
(load the page, check the network trace) before trusting the staged image, since
a stale bundle can look correct on disk while serving old assets.

## 3. Transfer

Copy the bundle tree (or the `.tar.zst`, then extract it) onto the USB drive or
other media you'll carry to the air-gapped venue, then onto each
server/worker/contestant machine. Run these from inside the transferred bundle
directory — they are the bundle's own copies; there is no repo checkout on the
target. Every target-side command in this runbook is written as
`cd <bundle-dir>` followed by `./script.sh ... --bundle .`; do not substitute a
`release/airgap/...` repo path here. `install.sh` resolves sibling files (like
the worker's `native/live-boot-preflight.sh` preflight, step 6) relative to its
own location, so running a repo copy of `install.sh` instead of the bundle's own
copy makes that lookup fail and the check silently skip instead of actually
running.

Transfers to removable media can silently truncate or corrupt files, so **before
trusting the media**, verify it against the manifest that was generated at
assembly time:

```bash
cd <bundle-dir>
./load-bundle.sh --bundle . --verify-only
```

This re-hashes every file in the bundle (except `manifest.sha256` itself) and
diffs the result against `manifest.sha256`. It exits non-zero and prints
`ABORT: bundle integrity check failed` on any mismatch — do not proceed with an
install until this passes. The same verification runs again, unconditionally, as
part of the contestant install (step 7) as a second trust-boundary check after
the media has changed hands.

## 4. Guided install (recommended)

`setup.sh` is the recommended one-click install path: it wraps the per-role
`install.sh` engine documented in §5–§7 with a runtime-aware preflight,
auto-generated secrets kept consistent across `compose/.env.infra` and
`compose/.env.server`, and compose service-name endpoints (so the server reaches
its co-located infra by service name — `db`/`redis`/`seaweedfs` — instead of you
wiring URLs by hand). It auto-detects Docker **and** Podman (override with
`--engine docker|podman` if both are present), is interactive by default, and is
fully scriptable via flags/environment variables for unattended installs. Like
every target-side script in this runbook, it makes zero network calls.

On the server, from inside the transferred bundle directory:

```bash
cd <bundle-dir>
./setup.sh --role server --bundle . --lan-host <host-or-ip> --admin-user admin
```

On each worker or contestant host, from inside the transferred bundle directory:

```bash
cd <bundle-dir>
./setup.sh --role worker --bundle . --cluster-secret <bundle>.cluster-secret
./setup.sh --role contestant --bundle .
```

The worker role additionally requires the cluster-secret sidecar delivered
alongside the bundle (see "Worker host (one-click)" below for the full flow);
`--worker-id` and `--lan-host` are both optional there — `--worker-id` defaults
to the host's short name, and `--lan-host` is only needed if the bundle wasn't
assembled with it already baked in. Without `--cluster-secret`, the worker
preflight fails.

The LAN hostname and admin username are also prompted for unless passed as flags
(`--lan-host`, `--admin-user`, default `admin`) — pass both, as in the example
above, and the server command prompts you for exactly one thing: the bootstrap
admin password (or pass it non-interactively with `--admin-pass` /
`$BROCCOLI_SETUP_ADMIN_PASS`). Everything else is generated locally with
`openssl rand` — no key is dropped, and the same secret value is kept consistent
across both files even where the key name differs. The Postgres and Redis
passwords are written as `POSTGRES_PASSWORD`/ `REDIS_PASSWORD` only in
`compose/.env.infra`; in `compose/.env.server` the same values are embedded
inside the `BROCCOLI__DATABASE__URL` / `BROCCOLI__MQ__URL` connection strings
rather than repeated as bare keys. The JWT secret (`BROCCOLI__AUTH__JWT_SECRET`)
and the S3 access/secret keys (`BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY` /
`..._SECRET_KEY`) are written identically, under the same key names, into both
files. Re-running `setup.sh` against an existing install reuses any real
(non-`change-me`) values already present in those files instead of rotating
them, so it is safe to re-run.

Two flags make `setup.sh` fit non-interactive workflows: `--dry-run` prints the
resolved plan (engine, compose provider, and the `install.sh` invocation it
would run) without deploying anything — on the server role it also writes the
generated env files first — and `--non-interactive` (paired with `--admin-pass`
/ `$BROCCOLI_SETUP_ADMIN_PASS` and any other required flags/env vars) skips
every prompt, for CI today and a future Ansible-driven install.

Before handing off to `install.sh`, `setup.sh` runs a go/no-go preflight: it
detects a working container engine (Docker or Podman) and, for the worker role,
the sandbox/isolate readiness. The preflight only detects and reports — it never
installs a runtime or touches the network — so a
`FAIL: no working docker or podman` line means you provision Docker (or Podman
on RHEL-family distros) out of band and re-run; `setup.sh` won't do it for you.

For the server role, the server-only secret sidecar still has to be delivered to
the server host exactly as in the manual path: place
`broccoli-airgap-<V>.server-secret/` as the bundle directory's sibling, or point
`setup.sh` at it explicitly with `--server-secret DIR` (see §8, `root.key`
security).

Once the preflight passes (and, on the server, the env files are generated),
`setup.sh` hands off to `install.sh`, which loads the images and brings the
stack up exactly as documented in §5–§7.

## 5. Server install

Directly running `install.sh --role server` is the underlying engine that
`setup.sh` (§4) wraps — use it this way only if you are managing the `.env`
files yourself.

Before running the installer, the operator must supply real environment files —
the bundle only ships `.example` templates so no secrets ever sit in a
distributable tarball. On the server machine, from inside the bundle directory:

```bash
cd <bundle-dir>
cp compose/.env.infra.example compose/.env.infra
cp compose/.env.server.example compose/.env.server
```

Then edit both files and fill in real values, at minimum:

- Postgres and Redis passwords (`POSTGRES_PASSWORD`, `REDIS_PASSWORD` in
  `.env.infra`, mirrored into `BROCCOLI__DATABASE__URL` / `BROCCOLI__MQ__URL` in
  `.env.server`).
- `BROCCOLI__AUTH__JWT_SECRET` (at least 32 characters).
- SeaweedFS/S3 credentials (`BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY` /
  `..._SECRET_KEY`, and the endpoint if it differs from the compose default).
- `BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD` for the initial admin account.
- `BROCCOLI_SERVER_IMAGE`, pointed at the image tag that was baked and loaded
  into this bundle.

The server also needs the server-only sidecar delivered to it: place
`broccoli-airgap-<V>.server-secret/` (the CA + leaf private keys minted at
assembly) next to the bundle directory as its sibling, or point at it explicitly
with `--server-secret DIR`.

`install.sh --role server` checks for `compose/.env.infra` and
`compose/.env.server` **before** doing any other work (loading images, issuing
the TLS leaf), and also fails fast if the server-secret directory is missing —
it will not silently bring up containers with empty or placeholder config. Once
the env files and the sidecar are in place, from the same bundle directory:

```bash
./install.sh --role server --bundle . --lan-host <host-or-ip>
```

This:

1. Runs `load-bundle.sh --bundle .` to re-verify `manifest.sha256` and
   `docker load` every tarball in `images/`.
2. Ensures the server-secret dir holds a current TLS leaf for `<host-or-ip>`.
   A pre-issued or earlier leaf is kept only while it chains to the bundle's
   CA, its SAN covers `<host-or-ip>`, and it is valid for at least another
   day; otherwise `ca/issue-leaf.sh` issues a new one from the CA key that
   lives only in the sidecar, so a venue whose address moved gets a matching
   leaf. The root and the leaf are both backdated a day, so a contestant
   machine whose clock runs behind does not reject them as not yet valid, and
   the leaf never outlives the CA.
3. If `--burn-ca-key` was passed, deletes `root.key` from the server-secret
   sidecar right after the leaf is issued (see §8).
4. Brings up infra, server, AND the Caddy TLS gateway (443, serving the
   internal-CA leaf, reverse-proxying to the server) with
   `docker compose -p compose --env-file .env.infra --env-file .env.server -f docker-compose.infra.yaml.template -f docker-compose.server.yaml.template -f docker-compose.server-airgap.yaml.template -f docker-compose.gateway-airgap.yaml.template up -d --pull never`
   — the Caddyfile is mounted un-rendered and Caddy expands its variables from
   the container environment; `--pull never` guarantees Compose only uses the
   images already loaded from `images/*.tar`, never reaching for a registry.
5. Makes the gateway the **only LAN ingress**: the server's plaintext `:3000` is
   bound to host loopback (`BROCCOLI_HTTP_BIND=127.0.0.1:3000`), so no
   contestant on the LAN can reach it and bypass TLS — HTTPS on 443 is the sole
   entrypoint. Host-local `curl http://127.0.0.1:3000/...` still works for
   operator troubleshooting. The server override
   (`docker-compose.server-airgap.yaml.template`) runs the server with
   `SECURE_COOKIES=true`, and the server trusts `X-Forwarded-For` only from the
   gateway, so auth rate limiting sees each contestant's real IP rather than
   the gateway's. The gateway and the server share a small `edge` network
   (`BROCCOLI_EDGE_SUBNET`, default `172.31.250.0/28`) that holds nothing else,
   and that subnet is the whole trust list; no LAN range is ever trusted.

   To run the gateway on a separate host instead (e.g. several servers), on the
   gateway host compose `docker-compose.gateway-airgap.yaml.template` alone,
   with `LAN_HOST`, `BROCCOLI_TLS_CERT`, `BROCCOLI_TLS_KEY` and
   `BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000'` exported. On each server
   host, export `BROCCOLI_HTTP_BIND=0.0.0.0:3000` and
   `BROCCOLI_GATEWAY_HOSTS=<gateway LAN IP>` before running `install.sh`: the
   server is then reachable on the LAN at `:3000` and trusts only that address.
   The airgap installer has no gateway-only role yet, and a second server host
   still needs `BROCCOLI_DB_HOST`, `BROCCOLI_MQ_HOST` and `BROCCOLI_S3_HOST`
   pointed at the first server's infra.

## 6. Worker install

Directly running `install.sh --role worker` is the underlying engine that
`setup.sh` (§4) wraps — use it this way only if you are managing the `.env`
files yourself. On each judging box, from inside the transferred bundle
directory:

```bash
cd <bundle-dir>
./install.sh --role worker --bundle .
```

This loads the worker image via `load-bundle.sh`, then runs the host half of
the go/no-go check staged inside the bundle at `native/live-boot-preflight.sh
--host-only` (arch/OS, systemd, cgroup-v2 controllers). isolate and the
compilers ship inside the worker image, so the host is not checked for them.
`install.sh` locates that preflight script
relative to its own path, so this step only fires when you run the bundle's own
`install.sh` as shown above — running a repo checkout's `install.sh` against the
same bundle directory makes `native/live-boot-preflight.sh` resolve to a
nonexistent path relative to the repo, and the check silently skips instead of
running. A failed preflight only warns
(`WARN: worker host preflight reported issues`) — inspect its `[FAIL]` lines
and fix them before trusting the box for judging, but it does not by itself
abort the install.

A worker does not trust the bundle's `ca/root.crt`: it talks plain Postgres,
Redis and S3 to the server host and never connects to the TLS gateway, so the
install works the same on Debian, RHEL-family and other hosts.

## Worker host (one-click)

Deliver the main bundle AND the `<bundle>.cluster-secret` sidecar to the worker
host (the sidecar carries the shared DB/redis/S3 credentials; it never goes to
contestants). Then:

    ./setup.sh --role worker --bundle broccoli-airgap-<version> \
        --cluster-secret broccoli-airgap-<version>.cluster-secret \
        --worker-id worker-a           # optional; defaults to this host's name

If the bundle was built with `--lan-host`, the server address is baked into the
sidecar and `--lan-host` may be omitted; otherwise pass
`--lan-host <server-ip>`. Each worker MUST use a distinct `--worker-id` — two
workers sharing an id corrupt heartbeat and dedup bookkeeping.

The worker connects to the server's Postgres/Redis/SeaweedFS over the LAN
(published on the server at ports 5432/6379/8333, password-gated). No image is
pulled; the worker image is loaded from the bundle and Compose runs
`--pull never`.

## 7. Contestant machines

Directly running `install.sh --role contestant` is the underlying engine that
`setup.sh` (§4) wraps — use it this way only if you are managing the `.env`
files yourself.

### Linux / macOS

On each contestant machine, from inside the transferred bundle directory:

```bash
cd <bundle-dir>
./install.sh --role contestant --bundle .
```

This re-runs `load-bundle.sh --bundle . --verify-only` first — a second,
independent `manifest.sha256` check at the point the media reaches the
contestant machine, since the trust boundary crosses again here. It then trusts
`ca/root.crt` via the appropriate per-OS `trust-ca/` helper (`linux.sh` installs
it with `update-ca-certificates` on Debian/Ubuntu/SUSE or `update-ca-trust` on
RHEL/Rocky/Fedora/Arch, as root or through `sudo`;
`macos.sh` adds it to the System keychain via `security add-trusted-cert`).
Finally, if `cli/broccoli` is present in the bundle, it installs the musl-static
CLI to `/usr/local/bin/broccoli`. The CLI trusts the OS certificate store by
default (alongside its bundled public roots), so `broccoli login` and
`broccoli submit` to `https://<host-or-ip>` work with no `BROCCOLI_TLS` setting.

### Windows

`install.sh` is a bash script and does not run on a native Windows box — its
contestant case also hard-exits with
`unsupported OS for contestant trust helper` on any `uname -s` other than
`Linux` or `Darwin`, so there's no path through `install.sh` for Windows at all.
Instead, run the trust helper directly in an **elevated (Administrator)
PowerShell**:

```powershell
cd <bundle-dir>
.\trust-ca\windows.ps1 ca\root.crt
```

This imports `ca/root.crt` into the Windows Local Machine Root store via
`certutil -addstore -f Root`. There is no CLI install step on Windows: the
bundled `cli/broccoli` binary is a Linux x86_64-musl static build and will not
run there. Windows contestants can still reach the server over HTTPS from a
trust-updated browser; they just need a different (or no) `broccoli` CLI.

### Firefox (all platforms)

**Firefox keeps its own certificate store**, separate from the OS trust store
that `trust-ca/` (or, on Windows, `windows.ps1` directly) populates. Every trust
helper prints a reminder of this; if contestants will use Firefox against the
LAN server, import `ca/root.crt` into Firefox separately (Settings → Privacy &
Security → Certificates → View Certificates → Authorities → Import).

## 8. root.key security

The client-distributed bundle tree **never** contains any private key. The CA
signing key `root.key` — which can mint new TLS leaves for the LAN's root CA —
and the leaf's `server.key` live **only** in the
`broccoli-airgap-<V>.server-secret/` sidecar (mode `0700`), delivered to the
server host alone. This is enforced structurally: `manifest.sha256` lists every
file in the bundle tree and `load-bundle.sh` re-verifies it, so a private key
cannot even be present-but-unlisted in what reaches a worker or contestant.

Because the CA key is powerful (anyone holding it can issue a leaf that
impersonates the server to any client that trusts `root.crt`), you have two ways
to keep it off the venue entirely:

1. **Burn after issue** — pass `--burn-ca-key` to `install.sh --role server`.
   The installer issues the server leaf as usual, then deletes `root.key` from
   the server-secret sidecar immediately afterward. Use this when you didn't
   pre-issue because you didn't know the venue's LAN host/IP until you arrived
   on-site.
2. **Pre-issue at assembly** — pass `--lan-host <host-or-ip>` to
   `build-bundle.sh` on the staging box (step 2) when you already know the
   venue's address ahead of time. The leaf is issued into the sidecar during
   assembly, so `install.sh --role server` finds a leaf that already covers
   that address and skips leaf issuance — you may then withhold
   `root.key` from the venue completely (deliver only `server.crt`/`server.key`
   in the sidecar, or drop `root.key` from it before copying to media).

Either way, only `ca/root.crt` (the public certificate, safe to distribute) is
ever installed on worker or contestant machines by `trust-ca` — now structurally
guaranteed, since no private key is ever part of the bundle tree they receive.

## 9. Upgrading a deployment

A new bundle version (a fresh `build-bundle.sh --version <V+1>` on the staging
box) rolls out **in place** — the postgres, redis, and SeaweedFS data live in
named Docker volumes that an image swap never touches, so every contest,
problem, submission, and uploaded file survives the upgrade.

On the **server** host:

1. Transfer and unpack the new bundle beside the old one (step 3). Loading its
   images does not disturb the running stack.
2. Carry your **existing** on-host config into the new bundle so secrets are
   never regenerated: copy `compose/.env.infra` and `compose/.env.server` from
   the old bundle into the new bundle's `compose/`, and deliver the same
   `broccoli-airgap-<oldV>.server-secret/` sidecar as the new bundle's
   `.server-secret/` (its CA and leaf stay valid — clients already trust
   `root.crt`). The new bundle's `.env.*.example` templates name the new image
   tags, but your carried-over `.env.server` still names the old one, so set
   `BROCCOLI_SERVER_IMAGE` (and, on workers, `BROCCOLI_WORKER_IMAGE`) to the new
   version — it must match `bundle.json`'s `version`.
3. Re-run `./install.sh --role server --lan-host <host-or-ip> --bundle .` from
   the new bundle. It `docker load`s the new images, reuses your secrets and
   leaf (no re-issue), and recreates only the containers whose image changed;
   the data volumes are reattached untouched.

Repeat on each **worker** host with `--role worker` (step 6). Server and worker
may run mixed versions briefly during a rolling upgrade — a new server judges
correctly against an as-yet-un-upgraded worker.

Confirm the swap took effect. The image carries its version, source commit and
build time in OCI labels — the only offline provenance signal, since there is
no registry to query. The commit matches `bundle.json`'s `git_sha`, so it also
answers whether a site runs a given fix:

```
docker inspect <server-container> --format \
  '{{index .Config.Labels "org.opencontainers.image.version"}} {{index .Config.Labels "org.opencontainers.image.revision"}} {{index .Config.Labels "org.opencontainers.image.created"}}'
```

then check that `https://<host>/healthz` returns `200`. To confirm the editor
works offline, open a problem page with the browser's developer tools on the
Network tab: it loads `/assets/editor.worker-*.js` from the server and makes
no request to `cdn.jsdelivr.net`.

> **Always upgrade with `install.sh`, never a hand-rolled `docker compose up`.**
> The infra secrets (`REDIS_PASSWORD`, the database and S3 credentials) live in
> `.env.infra`, and `install.sh` passes both `--env-file .env.infra` and
> `--env-file .env.server`. A manual `compose up` that omits `.env.infra`
> recreates redis with a blank password; the server's message-queue auth then
> fails and `/healthz` returns 503 (see §11, "Server won't start").

## 10. Rotating TLS certificates and the CA

TLS material rotates in place, like an upgrade — the gateway serves whatever
leaf is in the server-secret sidecar. How many machines you must touch depends
on **which** key changes.

The leaf is bind-mounted into the gateway at a fixed path, so Compose does not
recreate the gateway when only the file *content* changes, and Caddy does not
watch the cert files — a new leaf is served only after the gateway restarts.
`install.sh --role server` restarts the gateway for you on every run, so always
rotate by re-running it, never with a hand-rolled `docker compose up`.

### Leaf rotation (same CA)

Use this when the server leaf is near expiry or its SAN must change (e.g. the
venue's IP moved). The root CA — and therefore every client's trust — is
unchanged, so **no worker or contestant needs to do anything.** On the server,
with `root.key` still in the sidecar (do this before burning it):

```
./install.sh --role server --lan-host <host-or-ip> --bundle .
```

`install.sh` re-issues the leaf by itself when the address changed or the leaf
expires within a day. To force a new leaf (e.g. a few weeks before expiry),
issue it first:

```
./ca/issue-leaf.sh --ca-dir <server-secret-dir> --host <host-or-ip> \
  --out <server-secret-dir>
```

The re-issued leaf gets a new serial, still chains to the unchanged `root.crt`,
and the gateway serves it as soon as `install.sh` restarts it. Confirm with
`curl --cacert ca/root.crt https://<host>/healthz` — the *same* `root.crt`
clients already trust.

### CA rotation (new root)

Use this when the CA signing key is compromised or the root CA itself is
expiring. The trust anchor moves, so **every client must be re-trusted** — until
it holds the new `root.crt`, it rejects the server.

1. On the **staging box**, mint a fresh CA and issue a leaf from it:

   ```
   ./ca/mint-ca.sh --out <new-ca-dir>
   ./ca/issue-leaf.sh --ca-dir <new-ca-dir> --host <host-or-ip> --out <new-leaf-dir>
   ```

2. Assemble the new sidecar: `root.crt` from `<new-ca-dir>`, plus
   `server.crt`/`server.key` from `<new-leaf-dir>` (add `root.key` from
   `<new-ca-dir>` only if you will re-issue leaves on-site later). Replace the
   bundle's public `ca/root.crt` with the new one as well, so fresh installs
   trust it.
3. On the **server**, deliver the new sidecar and re-run
   `./install.sh --role server --lan-host <host-or-ip> --bundle .`. The gateway
   restarts onto the new leaf.
4. Re-distribute the new `ca/root.crt` to **every contestant machine** and
   re-run their trust step: `install.sh --role contestant`
   re-trust from the bundle's `ca/root.crt`; Firefox users re-import it (step 7).

A client still holding only the old `root.crt` fails the handshake with an
`unable to get local issuer certificate` error — that is the rotation working,
not a misconfiguration. It clears once the client trusts the new root. (Workers
reach postgres/redis/SeaweedFS directly on the LAN, not through the 443 gateway,
so a CA rotation does not interrupt in-flight judging.)

### After `--burn-ca-key`

Burning deletes `root.key` from the server host, so you cannot re-issue a leaf
there. Keep the CA (`root.key`) on the offline staging box: mint or issue a
replacement there and redeliver the sidecar as above. The running gateway keeps
serving its existing leaf until you do — burning the CA key never interrupts a
live TLS endpoint.

## 11. Troubleshooting

**`ABORT: bundle integrity check failed` / `manifest verification failed`** —
the copy onto removable media was corrupted or incomplete (or the tree was
edited after `manifest.sha256` was generated). Re-copy the bundle from the
staging box (or re-extract the `.tar.zst`) and, from inside the bundle
directory, re-run `./load-bundle.sh --bundle . --verify-only` before doing
anything else. Do not attempt to "fix" a mismatched manifest by hand —
regenerate the bundle.

**Browser/client shows a TLS warning even though the CA was trusted** — the
server's leaf certificate's SAN only covers the host/IP it was issued for. If
contestants reach the server via a different hostname or IP than the one passed
to `--lan-host` (at either `build-bundle.sh` or `install.sh --role server`
time), the leaf won't match. On the server, re-issue the leaf for the correct
host into the server-secret sidecar with
`./ca/issue-leaf.sh --ca-dir <server-secret-dir> --host <correct-host> --out <server-secret-dir>`
(requires `root.key` in the sidecar, so do this before burning it), then re-run
`./install.sh --role server --bundle . --lan-host <correct-host>` to bring the
gateway back up with the corrected leaf.

**Browser trust warnings persist after running the trust helper** — most
browsers read the OS certificate store and pick up `trust-ca/`'s changes
immediately, but Firefox uses its own store (see step 7) and needs `root.crt`
imported separately. Confirm which store the browser is reading from before
assuming the trust helper failed.

**Worker preflight reports `[FAIL]` items** — `native/live-boot-preflight.sh`
does not block the install by itself, but a worker with failing isolate,
cgroup-v2, or toolchain checks will misjudge submissions. Fix the specific
`[FAIL]` line (e.g. install `build-essential`, boot with
`systemd.unified_cgroup_hierarchy=1`) and re-run the preflight before sending
real submissions to that worker.

**Server won't start / config looks empty** — check that `compose/.env.infra`
and `compose/.env.server` exist and are filled in (step 5);
`install.sh --role server` refuses to run without them, but if you brought up
the compose files by hand without `install.sh` you can end up with variables
expanding to empty strings.
