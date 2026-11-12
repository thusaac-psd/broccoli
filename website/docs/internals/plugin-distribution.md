---
title: Plugin distribution
sidebar_label: Plugin distribution
sidebar_position: 3
---

# Plugin distribution

A plugin reaches every server replica from one of two sources, and the `plugin`
row is the single record of which one the fleet runs
(`packages/server/src/plugin_distribution.rs`):

- Bundle: `plugin.plugins_dir/<id>/`, shipped on every node. Only ever read, so
  release templates mount it read-only.
- Upload: a plugin archive stored in the shared blob store
  (`packages/common/src/storage`, database or object storage backend) under its
  SHA-256 content hash. The row's `archive_hash` column names it.

`archive_hash IS NULL` means bundle. A non-null value means every server runs
that archive, even where its bundle has the same plugin id. Removing the upload
clears the column and the bundled copy takes over again.

Workers run no plugin code. Evaluators, checkers and contest types all execute
on the servers, and workers only run the sandbox operations the servers send
them, so a published version needs to reach servers only.

## Publishing is one row write

`POST /admin/plugins/upload` (`handlers/admin.rs`, `plugin_lifecycle::install`):

1. `PluginArchive::parse` validates the archive (`plugin_archive.rs`): at most
   256 MiB compressed, 512 MiB and 10,000 entries unpacked, regular files and
   directories only, one top-level directory named after a valid plugin id,
   no path component other than a plain name, no duplicate files, a
   `plugin.toml` that parses with valid routes and timers, every manifest path
   inside the plugin root, and the server and worker entry points present.
2. The bytes go to the blob store. `put` is content addressed, so the key is
   the hash `parse` computed.
3. Under the lifecycle lock, the archive is unpacked into this server's cache
   and loaded through the same `load_then_publish` path enable and reload use.
4. Only if that load succeeds does one upsert set `archive_hash`, `is_enabled`,
   the published `version` and `content_hash` fingerprint, and bump
   `generation`. A failed load publishes nothing and answers
   `422 PLUGIN_LOAD_FAILED`; the previous version keeps serving here.

Every other server picks up the new generation on its next reconcile tick
(`RECONCILE_INTERVAL`, 5 s). There is no queue traffic: convergence is the
existing DB-driven loop, and a tick that finds nothing changed does no file or
blob work.

`POST /admin/plugins/upload/inspect` runs step 1 only and reports the id,
version, requested host permissions and what the fleet runs under that id now,
so the admin UI can show the permissions before publishing.

## Materializing on each server

`plugin_distribution::locate` turns the row's source into a directory:

- Bundle: `plugins_dir/<id>/` if it holds a `plugin.toml`.
- Upload: `cache_dir/<archive hash>/<id>/`. On a miss the server fetches the
  blob, refuses anything larger than an upload may be, recomputes the SHA-256
  and refuses a mismatch, runs `PluginArchive::parse` again (a stored archive is
  never trusted more than an uploaded one), checks the archive holds the
  expected plugin id, then unpacks into `cache_dir/.partial-<hash>` and renames
  it into place. A cache directory that exists is therefore complete.

`plugin.cache_dir` is per server and must be writable; nothing in it is
authoritative, so losing it costs one download per uploaded plugin. All cache
writes and deletions happen under the lifecycle mutex, so unpacking and cleanup
never race on one server. After a fleet change, `retain_cache` deletes every
cache entry that is neither published for some plugin nor the root of a
registry entry. The second condition keeps the files of a version that is still
serving after a newer one failed to load: extism rebuilds pooled instances from
the WASM file, and web assets and translations are read from the plugin root.

## Failure and rollback

A server that cannot use what was published (blob missing, hash mismatch,
invalid archive, `init()` failure, uploads disabled on that server) goes through
the normal `apply` failure path:

- If it was already running the plugin, the previous version keeps serving and
  the replica reports `outdated` with the error.
- If not, it reports `failed`. Either way the plugin is in `not_ready`, so
  `/healthz` says `degraded` and the visibility kernel fails closed for what the
  plugin governs (the recorded `governs` column).
- It retries with backoff. An admin Reload bumps the generation, so every
  failing server retries at once.

Rollback is publishing again: upload the previous archive, or remove the upload.

## Removing an upload

`DELETE /admin/plugins/{id}/upload` (`plugin_lifecycle::remove_upload`):

- The serving replica's bundle has the plugin: one update sets
  `archive_hash = NULL` with the bundle's fingerprint and bumps the generation.
  If the plugin is enabled, the bundled copy is loaded here first, as for any
  change. The plugin keeps its enabled state.
- No bundled copy: the row is deleted and pending timers go with it. Each
  server's reconcile sees a row it applied disappear and forgets the plugin:
  registrations purged, registry entry dropped, cache cleaned. Plugin config and
  storage rows stay, so uploading the plugin again restores its data.

## Concurrency guards

Publishing writes are conditional on the source the serving replica loaded, so
a change made through one replica never silently undoes one made through
another: Enable and Reload only apply while `archive_hash` still equals the
source they loaded, and removing an upload only applies while that same upload
is published. A refused write answers `409 CONFLICT` and rolls this replica back
to what it had, like any unpublished change.

## Operator switch

`plugin.uploads_enabled = false` makes both upload endpoints answer
`403 PLUGIN_UPLOAD_DISABLED`, and makes `locate` refuse every uploaded source on
that server, so it runs bundled code only and fails closed for anything else.
Removing an upload stays allowed, since it only returns to the bundle.
`GET /admin/plugins/upload` reports the switch to the admin UI.

## Tradeoffs

- The archive is buffered in memory on upload and on fetch. The size limits
  bound that, and a plugin is small next to test data.
- Peers pick up a publish on their next poll rather than through a push. That
  costs up to one reconcile interval of latency and keeps the hot path free of
  any new per-request or per-tick work.
- An upload whose load fails here leaves its blob in the store. It is content
  addressed and referenced by nothing, so it is inert; deleting it would race a
  concurrent upload of the same bytes.
- A plugin removed from the fleet while some node still bundles it is
  rediscovered by that node on a later fleet change and registered again as
  bundled. Keep bundles identical across nodes.
