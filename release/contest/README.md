# Contest hand-off: export problems → bring up the contest cluster (offline LAN)

This directory is for handing a finished problem set from the **authoring/staging
server** to a **contest cluster** that runs on an isolated LAN (e.g. lab machines
in mainland China with **no internet**). It contains:

| File | Role |
|------|------|
| `export-problems.sh` | Dump all non-deleted problems (DB rows + referenced blobs) from a running server into one portable `.tar.gz`. |
| `import-problems.sh`  | Restore that archive into a fresh contest server. |
| `s3copy.py`           | Dependency-free (stdlib-only) SeaweedFS/S3 copy used by both. No `aws`/`pip`/internet needed. |
| `bring-up-worker.sh`  | Thin non-interactive wrapper around `../install.sh worker`. |

The infra/server/worker bring-up itself is done by the bundle's role installer
`../install.sh` (Postgres + Redis + **SeaweedFS** + server, then workers). This
runbook sequences the whole hand-off around it.

> **Verified:** the export→import round-trip (problem rows, `checker_source`,
> per-problem plugin config, communication manager blobs, and a 1.2 MB test-case
> blob) has been tested byte-for-byte on staging — problem and test-case rows
> restore md5-identical and blob SHA-256 matches its content-hash key.

---

## The three phases (airgap pattern)

```
  (A) BUILD  ── connected, OUTSIDE the airgap ──►  one self-contained bundle .tar
  (B) SHIP   ── USB / scp into the LAN ───────────►  every contest box
  (C) DEPLOY ── offline, on the LAN ─────────────►  infra+server box, then workers
```

China LAN reality baked in: **nothing is fetched at deploy time.** Docker images
are `docker load`-ed from the bundle (never pulled), the S3 copy is pure Python
stdlib (no `aws-cli`), and the clock check only warns. Before the contest, make the
infra box the LAN's time server and point every other box at it
([../docs/operator-runbook.md](../docs/operator-runbook.md#time-sync-on-an-offline-lan)):
servers and workers that disagree on the time delay or lose judging results.

---

## (A) Build the bundle — connected machine, before the contest

The bundle's `install.sh` loads `images/*.tar.gz` and only falls back to a
registry pull if an image is missing — so the bundle must ship **every** image
built from the contest source branch (server, worker-icpc, postgres, redis,
seaweedfs, caddy). Build it where you have internet (crates/toolchain/base
images), `docker save` the images into the bundle, and include this `contest/`
directory plus the exported problem archive.

> ⚠️ The images must be built from the **contest branch** (the one carrying the
> judge fixes + contestant-CLI + printing-plugin), not `master`. Building from
> the wrong ref ships a different, untested judge. See "Open items" at the end.

## (A′) Export the problems — on the staging/authoring server

Run on the box that has the authored problems (reads DB + S3 creds straight from
its `config.toml`):

```bash
./export-problems.sh                 # writes broccoli-problems-<UTC>.tar.gz
#   --config PATH    broccoli config.toml (default /data/broccoli/config/config.toml)
#   --out PATH       archive path
#   --all-blobs      ship the whole bucket instead of only referenced blobs
```

It dumps **non-deleted** problems only (`problem`, `test_case`,
`problem_attachment`, `additional_file`) plus plugin config (global `plugin`
scope + per-problem `problem` scope) and exactly the object-storage blobs those
rows reference. It does **not** export contests, users, or submissions — the
admin re-creates the contest and accounts on the contest server.

Put the resulting `broccoli-problems-*.tar.gz` into the bundle (or carry it
alongside).

## (B) Ship

Copy the bundle `.tar` (images + `install.sh` + compose templates + plugins +
`broccoli-compare` + this `contest/` dir + the problem archive) onto every
contest box over USB or LAN scp. Extract to the same directory on each.

## (C) Deploy on the LAN — offline

### Box 1 — infra + server (Postgres, Redis, SeaweedFS, API)

```bash
cd <bundle>
export BROCCOLI_INFRA_HOST=<box1-LAN-IP>     # so workers reach DB/MQ/S3 over the LAN
./install.sh infra server
#   - choose object_storage (SeaweedFS) at the storage menu  [recommended]
#   - writes connection.env and server-secrets.env in this dir
```

The server auto-creates its schema and registers plugins on first boot. Then
load the problems from any box the installer set up. The script reads the
`connection.env` the infra install wrote (Docker or native) from the bundle
directory, and runs `psql` from the host when it has one, otherwise from the
bundle's Postgres image through Docker:

```bash
cd contest
./import-problems.sh ../broccoli-problems-*.tar.gz
#   --env-file PATH a connection.env elsewhere than ../connection.env
#   --truncate      replace problems if the target already has some
```

Verify it prints `problems=<N> (manifest <N>)` and `blobs_loaded=<M>`.

### Boxes 2..N — workers

Copy `connection.env` from Box 1 into each worker's bundle dir, then:

```bash
cd <bundle>
# (connection.env from box 1 is in this dir)
./install.sh worker     # the worker id defaults to one unique to this machine
#   choose the 'icpc' worker image (C/C++/Python) at the worker-image menu
```

`bring-up-worker.sh` wraps this non-interactively for fleets:

```bash
./contest/bring-up-worker.sh            # reads ./connection.env; --id NAME to pick the id
```

### Sanity check

From any box on the LAN: open `http://<box1-LAN-IP>/`, log in as admin, confirm
the problem list, and submit one solution to confirm a worker picks it up.

---

## Whole-contest bundle

Beyond a plain problem-set archive, `export-problems.sh`/`import-problems.sh` can
also carry a **contest** end-to-end: the contest row itself, its roster, and the
roster's accounts/roles — for standing up a duplicate/rehearsal contest, or for
disaster recovery of a single contest without a full-database restore.

```bash
# on the source server
./export-problems.sh --contest 42 [--with-secrets] --out contest-42.tar.gz

# on the target server (must have booted at least once already, so its
# schema exists -- these scripts never create schema themselves)
./import-problems.sh --bundle DIR [--with-secrets] [--truncate]
#   (or: ./import-problems.sh contest-42.tar.gz ...)
```

- `--contest ID` on export additionally dumps `contest`, `contest_problem`,
  `contest_user`, `user`, `user_role`, `role`, `role_permission` (only the rows
  reachable from that contest's roster) and tags the archive's manifest
  `"format": "broccoli-contest/v1"` instead of `broccoli-problems/v1`.
  `import-problems.sh` detects this tag and restores those tables too, in
  FK-safe order, alongside the usual problem tables.
- **Accounts are never clobbered.** The user upsert is
  `ON CONFLICT (username) WHERE deleted_at IS NULL DO NOTHING`: an existing
  **active** account (`deleted_at IS NULL`) is always left untouched, even by a
  blank or stale password from the bundle. Only brand-new usernames get
  inserted. This makes a contest bundle safe to import onto a target that
  already has some of the same operators/contestants registered.
- **Secrets travel only with `--with-secrets`, and only at export time.**
  Without it, every `user.password` in the bundle is blanked (`''`) before it
  ever leaves the source — the archive itself carries no credentials. Passing
  `--with-secrets` to `import-problems.sh` is accepted for symmetry but is a
  **documented no-op**: the upsert above never overwrites an existing active
  user's password regardless of this flag, and a freshly-inserted user simply
  gets whatever password column the bundle happened to carry (real hash if
  exported `--with-secrets`, blank otherwise). Whether real secrets are in the
  bundle is decided once, at export time.
- The target's schema must already exist — i.e. **the target server has
  booted at least once** (entity `sync()` + `Migrator::up` create it on boot).
  These scripts only read/write rows; they never create tables.
- `--dry-run` on either script touches nothing: `export-problems.sh --dry-run`
  never opens a DB connection or reads `--config` at all (it just prints the
  `\copy` statements it would run), and `import-problems.sh --dry-run` prints
  the `restore.sql` it would run and, for a tarball argument, peeks the
  manifest with a read-only `tar` extract of just `manifest.json` — no
  extraction of the rest of the archive, no config read, no DB, no S3.

## Notes / gotchas

- **SeaweedFS volume capacity.** Blobs live in a per-bucket *collection*; the
  first write needs a volume. `import-problems.sh` retries transient 5xx while
  SeaweedFS auto-grows one. If imports fail with "not enough volumes", the
  seaweedfs `volumeSizeLimitMB` / max-volumes is too low for the dataset — raise
  it in the infra config before importing.
- **Postgres version.** Staging dumps are plain `COPY` data (PG 14) and restore
  into the bundle's Postgres (PG 18) — the forward-compatible direction.
- **Credentials.** `export` reads DB + S3 creds from the staging server's
  `config.toml`, so run it as a user that can read it. `import` reads them from
  `connection.env`, so run it as a user that can read that file (and Docker,
  when the box has no `psql`).
- **testlib special judges** carry their `testlib.h` inside `checker_source`
  (stored in the DB), so they export/import intact with no internet — the
  `fetch-testlib.sh` in `examples/testlib-checker` is only for authoring new
  problems on a connected machine.

## Open items before this is contest-final
- Build the bundle images from the **rebased contest branch** (master's
  contestant-CLI + printing-plugin merged in), then re-run the judge test
  battery.
- Distribute/point the **contestant CLI** at `http://<box1-LAN-IP>` and wire the
  **printing plugin** to the contest printer (LAN).
- Decide whether Box 1 should also judge (`install.sh single-host` adds a local
  worker) or stay infra+server only.
