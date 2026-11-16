# Contest hand-off: export problems → bring up the contest cluster (offline LAN)

This directory is for handing a finished problem set from the
**authoring/staging server** to a **contest cluster** that runs on an isolated
LAN (e.g. lab machines in mainland China with **no internet**). It contains:

| File                 | Role                                                                                                      |
| -------------------- | --------------------------------------------------------------------------------------------------------- |
| `export-problems.sh` | Write the non-deleted problems (rows and the blobs they reference), or a whole contest, to one `.tar.gz`. |
| `import-problems.sh` | Restore that archive onto another server.                                                                 |
| `bring-up-worker.sh` | Thin non-interactive wrapper around `../install.sh worker`.                                               |

Both scripts run `broccoli-server bundle export|import` on the server node: the
native install's binary from its data directory, or the running server container
through `docker exec -i`. The command uses the server's own configuration, so it
reads and writes every storage backend (`database`, `filesystem`,
`object_storage`) and names each table's columns from the live schema.

The infra/server/worker bring-up itself is done by the bundle's role installer
`../install.sh` (Postgres + Redis + **SeaweedFS** + server, then workers). This
runbook sequences the whole hand-off around it.

The integration test
`bundle::a_contest_bundle_restores_every_row_and_blob_onto_another_server`
exports a seeded contest and restores it onto a second server, comparing every
row and blob.

---

## The three phases (airgap pattern)

```
  (A) BUILD  ── connected, OUTSIDE the airgap ──►  one self-contained bundle .tar
  (B) SHIP   ── USB / scp into the LAN ───────────►  every contest box
  (C) DEPLOY ── offline, on the LAN ─────────────►  infra+server box, then workers
```

China LAN reality baked in: **nothing is fetched at deploy time.** Docker images
are `docker load`-ed from the bundle (never pulled), and the export and import
run inside the server itself. No NTP is needed: servers keep time by the
database clock
([../docs/operator-runbook.md](../docs/operator-runbook.md#time-on-an-offline-lan)).

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

Run on the server node that has the authored problems:

```bash
./export-problems.sh                 # writes broccoli-problems-<UTC>.tar.gz
#   --out PATH       archive path
#   --problems 1,2   only these problems
```

It writes **non-deleted** problems only (`problem`, `test_case`,
`problem_attachment`, `additional_file`) plus plugin config (global `plugin`
scope and per-problem `problem` scope) and exactly the blobs those rows
reference. If a referenced blob is missing from storage it writes nothing, lists
the missing hashes and exits non-zero. Without `--contest` it does **not**
export contests, users, or submissions; see "Whole-contest bundle".

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

The server migrates its database and registers plugins on first boot. Then load
the problems on the server box:

```bash
cd contest
./import-problems.sh ../broccoli-problems-*.tar.gz
#   --replace-problems  replace problems if the target already has some
#   --wipe-dependents   with --replace-problems, also delete the submissions,
#                       code runs and contest links that reference them
```

It prints each table's row count and the number of blobs stored. The rows go in
one transaction, committed after every blob is stored, so a failed import
changes no row.

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

Beyond a problem set, the archive can carry a **contest** end to end: the
contest row, its plugin config, its roster, and the roster's active accounts
with their roles. Use it to stand up a rehearsal contest, or to move one contest
to another server.

```bash
# on the source server
./export-problems.sh --contest 42 [--with-secrets] --out contest-42.tar.gz

# on the target server
./import-problems.sh contest-42.tar.gz [--replace-problems]
```

- **Accounts merge by username.** An active account the target already has keeps
  its id, password and profile; the bundle's roster entries and roles follow it.
  Other accounts are added with new ids.
- **Roles never widen.** A role the target lacks arrives with its permissions; a
  role it already has keeps exactly its own permissions.
- **Secrets travel only with `--with-secrets`.** Without it every password in
  the archive is blank, so the archive carries no credentials and a new account
  cannot log in until an admin sets its password.
- **Schema versions.** The archive records the source database's schema version
  and each table's columns. It restores onto a server of the same or a newer
  build, which has started once (so its database is migrated); a column the
  source lacked takes its default. A build older than the source refuses the
  archive.

## Notes / gotchas

- **SeaweedFS volume capacity.** Blobs live in a per-bucket _collection_; the
  first write needs a volume. If imports fail with "not enough volumes", the
  seaweedfs `volumeSizeLimitMB` / max-volumes is too low for the dataset; raise
  it in the infra config before importing.
- **Postgres version.** The archive holds `COPY` text, which restores into the
  same or a newer Postgres major version.
- **Where to run.** Run both scripts on the server node, as a user that can read
  the native install's `config/config.toml` or use Docker. With more than one
  server container on the host, set `BROCCOLI_SERVER_CONTAINER` to the one to
  use.
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
