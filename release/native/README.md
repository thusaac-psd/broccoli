# Native contest bundle (offline LAN, no Docker)

Deploys Broccoli onto airgapped Ubuntu 22.04 (x86_64) lab machines **without
Docker** — native Postgres + Redis + SeaweedFS + systemd-managed broccoli
services + isolate. This mirrors the battery-verified staging deployment exactly
(same binaries, same systemd units, same isolate cgroup-v2 setup).

Use this instead of the Docker bundle (`../install.sh`) when the lab machines
have no Docker / no Docker Hub access (typical in mainland China).

| File | Role |
|------|------|
| `build-native-bundle.sh` | Run on a **built** box to assemble the bundle tarball. |
| `install-native.sh`      | Deploy a role on a fresh box: `infra-server` or `worker`. |
| `systemd/`, `config/`    | Unit + config templates (filled in at install time). |
| `contest/`               | Problem export/import (added into the bundle). |
| `cli/broccoli`           | Contestant CLI binary — copy out to contestant machines. |

## (A) Build the bundle — on a built x86_64 Ubuntu box (e.g. staging)

The box must already have: compiled `target/release/{server,worker,broccoli-compare}`,
installed `/usr/local/bin/{weed,isolate}`, built plugin `.wasm` under
`plugins/*/`, and the server web build under `packages/web/build/client`.

```bash
cd release/native
# optional but recommended for live-boot/airgapped workers: pre-stage the
# C/C++/Python toolchain as offline .debs (needs Docker + internet, once):
./stage-toolchain.sh               # -> toolchain/{20.04,22.04,24.04}/*.deb
./build-native-bundle.sh v1        # -> broccoli-native-v1.tar.gz (+ .sha256)
```

The bundle contains the binaries, the `.wasm` judge plugins, the server web UI,
`weed` + `isolate`, the systemd unit + config templates, the export/import
tooling, `stage-toolchain.sh`, and any pre-staged `toolchain/` `.deb` sets.
~700 MB (binaries carry line-table debug info for backtraces).

> **testlib.h / special judges** ride inside each problem's `checker_source` in
> the DB, so they survive export/import with no internet.

## (B) Ship

Copy `broccoli-native-v1.tar.gz` onto every contest box over USB or LAN scp.
Extract to the same directory on each (`tar xzf broccoli-native-v1.tar.gz`).

## (C) Deploy — offline, on the LAN

Lab boxes are Ubuntu 22.04. Postgres/Redis install via apt; point apt at a local
mirror or pre-install `postgresql redis-server build-essential` if fully airgapped.

### Box 1 — infra + server (Postgres, Redis, SeaweedFS, API)

```bash
cd broccoli-native-v1
sudo BROCCOLI_INFRA_HOST=<box1-LAN-IP> ./install-native.sh infra-server
```

This installs Postgres (creates the `broccoli` role/DB + LAN access), Redis (LAN
bind + password), SeaweedFS (`weed`, S3 on `:8333`), and `broccoli-server`
(systemd, port 80). SeaweedFS's master, volume and filer listen on loopback
only; the LAN reaches just the credentialed S3 API, which
`broccoli-weed-s3.socket` forwards from `<box1-LAN-IP>:8333`. Postgres (5432),
Redis (6379) and S3 (8333) still accept LAN connections with passwords, so
on a venue LAN, firewall them from contestant subnets and allow only the
worker boxes. It **generates all secrets** on the first run (a re-run, e.g. to update the
binaries, keeps them; `--rotate-secrets` makes new ones and then every worker
needs the new `connection.env`), prints the admin password, and
writes `connection.env` for the workers. The server auto-creates its schema and
loads the plugins on first boot.

Then load the problems (exported from the authoring server with
`contest/export-problems.sh`):

```bash
cd contest
sudo ./import-problems.sh ../../broccoli-problems-*.tar.gz --config /data/broccoli/config/config.toml
```

### Boxes 2..N — workers

Copy `connection.env` from box 1 into each worker's bundle dir, then:

```bash
cd broccoli-native-v1
sudo ./install-native.sh worker   # id unique to this box; BROCCOLI__WORKER__ID=... to choose one
```

This installs `isolate` (setuid + cgroup-v2 delegation via the per-boot
`broccoli-isolate-setup` service), the **C/C++/Python toolchain**, and
`broccoli-worker` pointed at box 1 over the LAN. `max_concurrency` defaults to 8
(override with `BROCCOLI_MAX_CONCURRENCY`).

Toolchain provisioning (`ensure_toolchain`) tries, in order: a bundled `.deb`
set matching the box's Ubuntu version (`toolchain/<ver>/`, see live-boot below),
then `apt` (online or a LAN mirror). It then **compile-tests** `gcc`/`g++` and
refuses to bring up a worker that can't compile C/C++ — a compiler-less worker
would wrongly `CE` every C/C++ submission. (Deliberate Python-only pool? re-run
with `BROCCOLI_ALLOW_NO_CXX=1`.)

### Ubuntu live-boot workers (20.04 / 22.04 / 24.04)

Workers are **stateless** (they pull jobs from box 1's queue), so booting them
from a Ubuntu live USB is fine — just re-run `install-native.sh worker` each
boot. cgroup-v2 sandboxing is verified per boot by `broccoli-isolate-setup`;
networking bridges to the real LAN (no NAT, unlike WSL2). **Box 1 must NOT be
live-boot** — it holds the DB/problems/submissions and must survive a reboot;
install it persistently (disk install or a USB with a persistence partition).

The one gap: the Ubuntu **Desktop live ISO ships `python3` but not `gcc`/`g++`**.
Provide the toolchain by one of:

- **Bundled `.debs` (offline):** pre-stage them once on any online machine with
  Docker — `./stage-toolchain.sh` writes `toolchain/{20.04,22.04,24.04}/*.deb`,
  folded into the bundle by `build-native-bundle.sh`. On the live-boot worker the
  installer auto-runs `dpkg -i toolchain/$(. /etc/os-release; echo $VERSION_ID)/*.deb`.
- **apt / LAN mirror (online):** `sudo apt-get install -y build-essential python3`.
- The installer falls back to apt automatically, then the compile-test gate.

> The staged closure is resolved against a minimal base image, so it is a
> superset of what a full Desktop ISO needs (extras dpkg-skip). Within one LTS
> this is robust; across point-release skew the apt/mirror path stays correct.

After installing a worker, get a **go/no-go** for the machine — it checks
x86_64 + systemd, cgroup-v2 controllers, the isolate **MLE/TLE verdicts** (live
probe), and a real C/C++/Python compile:

```bash
sudo ./live-boot-preflight.sh <box1-LAN-IP>   # exit 0 = GO, nonzero = NO-GO
```

### Contestant CLI (optional, for contestants)

The bundle ships `cli/broccoli` — a self-contained x86_64 client binary
(`login`, `submit`, `test`, `contest`, `status`, `watch`, …). It links only
glibc + libgcc, so it runs on any Ubuntu 22.04 lab machine with no extra
packages. Distribute it to contestant machines over the LAN:

```bash
scp broccoli-native-v1/cli/broccoli contestant-box:/usr/local/bin/broccoli
# on a contestant box:
broccoli login                       # point at http://<box1-LAN-IP>/
broccoli submit sol.cpp -p A -c <id> # submit
broccoli test sol.cpp -p A -c <id>   # run samples locally/remote
```

### Print stations (optional, for code printing)

The bundle ships `cli/print-client` — the print-station client. Run it on each
machine that has a printer; it polls the server, renders syntax-highlighted PDFs
(CJK font bundled), and runs the print command. Verified end-to-end: PDF
generation + folder sink + `lp`-style command invocation + job status reporting.

1. In the web UI (admin → print plugin config, **plugin** scope), add one or more
   **station tokens**.
2. On each station: copy `cli/print-client` + `cli/print-client.toml.example` →
   `print-client.toml`, set `[[server]].url`/`token` and `[[printer]].command`
   (`lp -d <printer> {file}` for CUPS, or `folder:/path` to drop PDFs to a folder).
3. `print-client doctor` to check connectivity, then `print-client run`.

> Each rendered PDF is ~25 MB — the bundled CJK font is embedded un-subsetted.
> Prints fine; just size temp/spool space accordingly.

### Verify

From any LAN box: open `http://<box1-LAN-IP>/`, log in as admin, confirm the
problem list, submit one solution, confirm a worker judges it.

## Notes / limitations
- **Plugin web UIs**: the bundle ships every judge `.wasm` (judging works) plus
  the **print** plugin's staff web UI (`plugins/print/web/dist/` — print queue
  panel + print buttons) and its backend `.wasm`. The other plugins' optional
  config-UI panels are only included if their frontends were built. Building any
  plugin frontend requires building `packages/web-sdk` first (it emits
  `dist/plugin.css` that every plugin frontend `@import`s), then
  `pnpm --filter <plugin> build`. Authors can configure plugins via the API
  meanwhile; rebuild the remaining frontends on a connected machine for full UI.
- **SeaweedFS volumes**: if `import` fails with "not enough volumes", raise the
  master volume limit before importing (the dataset needs a volume per bucket).
- **Postgres version**: bundle dumps restore into the box's Postgres; staging
  dumps are PG14. Keep the lab on PG14+ (Ubuntu 22.04 ships 14).
- **Not yet 2-box-tested from the build host**: the export/import round-trip and
  the judging battery are verified; the multi-box install path is assembled from
  the proven single-box recipe but should be dry-run on two boxes before the
  contest.
