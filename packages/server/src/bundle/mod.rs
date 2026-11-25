//! Problem and contest bundles: a server's problems (and, for a contest, its
//! roster and their accounts) as one portable `.tar.gz` that another server
//! restores. `broccoli-server bundle export|import` ([`cli`]) runs them with the
//! server's own configuration, so they read the database through the live
//! schema and blobs through whichever storage backend the server uses.
//!
//! The archive holds, in this order:
//!
//! - `manifest.json`, the [`Manifest`];
//! - `db/<table>.copy` for each table, its rows in Postgres `COPY` text format,
//!   with the columns the manifest lists for it;
//! - `blobs/<hash>` for each blob those rows reference.
//!
//! No bundle carries a secret config field (`secret = true` in its plugin's
//! schema): the export withholds each one as the read routes do and lists
//! where in [`Manifest::withheld`]. An import onto the installation that
//! wrote the bundle ([`Manifest::installation`]) keeps what it stores there;
//! onto any other, a withheld secret is left unset, since another
//! installation's secret for a config of the same id was never this
//! bundle's.
//!
//! Columns are read from the database at export and named at import, so a
//! bundle restores onto any build that knows its schema version: a column
//! added since then takes its default.

pub mod cli;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail, ensure};
use common::db::DatabaseConnection;
use common::storage::{BlobStore, ContentHash};
use futures::TryStreamExt;
use sea_orm::sqlx::{self, AssertSqlSafe, Postgres, Transaction};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::SyncIoBridge;

use crate::entity::user::username_key_sql;
use crate::host_funcs::config::{extract_plugin_id, strip_namespace_prefix};
use crate::migration::{Migrator, known_migrations};
use crate::plugin_archive::PluginArchive;
use crate::services::plugin_config::ConfigScope;
use plugin_core::manifest::{ConfigNamespace, SecretStep, keep_withheld};

/// [`Manifest::format`] of every bundle this build writes and reads.
pub const FORMAT: &str = "broccoli-bundle/v1";

const MANIFEST: &str = "manifest.json";
const DB_DIR: &str = "db/";
const BLOB_DIR: &str = "blobs/";

/// What a bundle carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every problem that is not deleted.
    AllProblems,
    /// These problems, those not deleted.
    Problems(Vec<i32>),
    /// A contest: its problems, the contest itself, its roster, and the
    /// roster's active accounts with their roles.
    Contest(i32),
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub selection: Selection,
    /// Carry the accounts' password hashes. Without it no account has a
    /// password, so the archive holds no credentials. Secret config fields
    /// are withheld either way.
    pub with_secrets: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// Replace every problem the target already has, those the bundle does
    /// not carry included. Refused while anything references them, unless
    /// `wipe_dependents`.
    pub replace_problems: bool,
    /// Replace the target's contest that has the bundle's contest id.
    /// Refused while anything but its roster and problem links references
    /// it, unless `wipe_dependents`.
    pub replace_contest: bool,
    /// With `replace_problems` or `replace_contest`, also delete every row
    /// that references what is replaced (submissions and their results, code
    /// runs, contest problem links, clarifications).
    pub wipe_dependents: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    /// The newest migration the source database had applied.
    pub schema: String,
    pub contest: Option<i32>,
    pub tables: Vec<TableRows>,
    /// The blobs under `blobs/`, by hash.
    pub blobs: Vec<String>,
    /// The secret config fields the export withheld.
    pub withheld: Vec<Withheld>,
    /// The id of the installation that wrote the bundle
    /// (`entity::installation`): only an import onto it keeps the secrets
    /// the export withheld.
    pub installation: String,
}

/// The secret fields of one `plugin_config` row that an export withheld:
/// `null` in the bundle at each of `secrets`, each named by identity: an
/// array item by its key field, never by its position, so the import keeps
/// what the target holds for the same item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Withheld {
    pub scope: String,
    pub ref_id: String,
    pub namespace: String,
    pub secrets: Vec<Vec<SecretStep>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRows {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: u64,
}

/// What an export wrote or an import restored.
#[derive(Debug, Default)]
pub struct Summary {
    pub tables: Vec<(String, u64)>,
    pub blobs: usize,
    /// The secret fields an import left unset: the bundle comes from another
    /// installation, so the secrets the target stores are not its own.
    pub secrets_unset: usize,
    /// The plugins, and the `plugin:namespace` configs, an export left out:
    /// no manifest of the plugin is at hand, or it does not declare the
    /// namespace, so no one could say which of their fields are secret.
    pub configs_left_out: Vec<String>,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (table, rows) in &self.tables {
            writeln!(f, "{table}: {rows} rows")?;
        }
        if self.secrets_unset > 0 {
            writeln!(
                f,
                "secret config fields left unset (the bundle comes from another installation): {}",
                self.secrets_unset
            )?;
        }
        if !self.configs_left_out.is_empty() {
            writeln!(
                f,
                "configs left out (no manifest of the plugin here): {}",
                self.configs_left_out.join(", ")
            )?;
        }
        write!(f, "blobs: {}", self.blobs)
    }
}

/// The rows of one bundle, as SQL over the source database.
struct Scope {
    /// `SELECT id FROM problem ...`: the problems the bundle carries.
    problems: String,
    contest: Option<i32>,
}

impl Scope {
    fn new(selection: &Selection) -> Self {
        let mut problems = "SELECT id FROM problem WHERE deleted_at IS NULL".to_string();
        let contest = match selection {
            Selection::AllProblems => None,
            Selection::Problems(ids) => {
                let ids: Vec<String> = ids.iter().map(i32::to_string).collect();
                problems.push_str(&format!(" AND id IN ({})", ids.join(", ")));
                None
            }
            Selection::Contest(id) => {
                problems.push_str(&format!(
                    " AND id IN (SELECT problem_id FROM contest_problem WHERE contest_id = {id})"
                ));
                Some(*id)
            }
        };
        Self { problems, contest }
    }

    /// The roster's active accounts.
    fn accounts(contest: i32) -> String {
        format!(
            "SELECT cu.user_id FROM contest_user cu JOIN \"user\" u ON u.id = cu.user_id \
             WHERE cu.contest_id = {contest} AND u.deleted_at IS NULL"
        )
    }
}

/// How a table's rows join the rows the target already has.
enum Restore {
    /// Plain inserts: import empties these tables of problems first.
    Insert,
    /// Rows merge on `key`. With `update` a bundle row replaces the target's
    /// row with the same key; without, the target's row stays.
    Merge {
        key: &'static [&'static str],
        update: bool,
    },
    /// Roles the target lacks are added. A role it has keeps its own
    /// permissions: a bundle never grants an existing role anything.
    NewRoles,
    /// The permissions of the roles [`Restore::NewRoles`] added.
    NewRolePermissions,
    /// Accounts merge by username, compared through
    /// [`crate::entity::user::username_key_sql`]. An active account the target
    /// has stays as it is, password and roles included; a new one gets a new
    /// id. Every bundle row that references an account follows it to its id on
    /// the target.
    Accounts,
    /// Rows that reference an account (`user_id`), moved to the account's id
    /// on the target, then merged on `key`. `of` says which accounts' rows
    /// are restored.
    AccountRefs {
        key: &'static [&'static str],
        of: AccountsOf,
    },
}

/// The accounts whose rows an [`Restore::AccountRefs`] table restores.
#[derive(Clone, Copy)]
enum AccountsOf {
    /// Only the accounts this import created: rows that would change what
    /// an existing account may do (its roles) never reach it.
    Created,
    /// Every account of the bundle that is active on the target, created or
    /// already there.
    Active,
}

struct Table {
    name: &'static str,
    /// The rows of this table in the bundle, as a `WHERE` condition; `None`
    /// when the bundle does not carry the table.
    rows: fn(&Scope) -> Option<String>,
    restore: Restore,
}

/// Every table a bundle can carry, in restore order (each after the tables
/// its rows reference).
const TABLES: &[Table] = &[
    Table {
        name: "role",
        rows: |s| {
            s.contest.map(|c| {
                format!(
                    "name IN (SELECT role FROM user_role WHERE user_id IN ({}))",
                    Scope::accounts(c)
                )
            })
        },
        restore: Restore::NewRoles,
    },
    Table {
        name: "role_permission",
        rows: |s| {
            s.contest.map(|c| {
                format!(
                    "role IN (SELECT role FROM user_role WHERE user_id IN ({}))",
                    Scope::accounts(c)
                )
            })
        },
        restore: Restore::NewRolePermissions,
    },
    Table {
        name: "user",
        rows: |s| s.contest.map(|c| format!("id IN ({})", Scope::accounts(c))),
        restore: Restore::Accounts,
    },
    Table {
        name: "user_role",
        rows: |s| {
            s.contest
                .map(|c| format!("user_id IN ({})", Scope::accounts(c)))
        },
        restore: Restore::AccountRefs {
            key: &["user_id", "role"],
            of: AccountsOf::Created,
        },
    },
    Table {
        name: "problem",
        rows: |s| Some(format!("id IN ({})", s.problems)),
        restore: Restore::Insert,
    },
    Table {
        name: "test_case",
        rows: |s| Some(format!("problem_id IN ({})", s.problems)),
        restore: Restore::Insert,
    },
    Table {
        name: "problem_attachment",
        rows: |s| Some(format!("problem_id IN ({})", s.problems)),
        restore: Restore::Insert,
    },
    Table {
        name: "additional_file",
        rows: |s| Some(format!("problem_id IN ({})", s.problems)),
        restore: Restore::Insert,
    },
    // The configs of the bundle's problems, contest and contest problems
    // (`models::plugin_config::config_key` names them), never a plugin-scope
    // config: those are each server's own settings, which an import leaves
    // as they are.
    Table {
        name: "plugin_config",
        rows: |s| {
            let mut rows = format!(
                "(scope = 'problem' AND ref_id IN (SELECT id::text FROM ({}) p))",
                s.problems
            );
            if let Some(c) = s.contest {
                rows.push_str(&format!(
                    " OR (scope = 'contest' AND ref_id = '{c}') OR (scope = 'contest_problem' \
                     AND ref_id IN (SELECT '{c}:' || id FROM ({}) p))",
                    s.problems
                ));
            }
            Some(rows)
        },
        restore: Restore::Merge {
            key: &["scope", "ref_id", "namespace"],
            update: true,
        },
    },
    Table {
        name: "contest",
        rows: |s| s.contest.map(|c| format!("id = {c}")),
        restore: Restore::Merge {
            key: &["id"],
            update: true,
        },
    },
    Table {
        name: "contest_problem",
        rows: |s| {
            s.contest
                .map(|c| format!("contest_id = {c} AND problem_id IN ({})", s.problems))
        },
        restore: Restore::Merge {
            key: &["contest_id", "problem_id"],
            update: false,
        },
    },
    Table {
        name: "contest_user",
        rows: |s| {
            s.contest
                .map(|c| format!("contest_id = {c} AND user_id IN ({})", Scope::accounts(c)))
        },
        restore: Restore::AccountRefs {
            key: &["contest_id", "user_id"],
            of: AccountsOf::Active,
        },
    },
];

/// Columns a bundle never carries and an import never restores: what the
/// target works out for itself. Whether a problem's configs have issues
/// depends on the plugins that check them, which an import does not run, so
/// an imported problem starts unknown: flagged and marked for a recheck (see
/// [`mark_for_recheck`]).
const DERIVED: &[(&str, &str)] = &[("problem", "config_issues"), ("problem", "config_recheck")];

fn derived(table: &str, column: &str) -> bool {
    DERIVED.contains(&(table, column))
}

/// The tables import empties before restoring problems.
fn problem_tables() -> impl Iterator<Item = &'static str> {
    TABLES
        .iter()
        .filter(|t| matches!(t.restore, Restore::Insert))
        .map(|t| t.name)
}

fn table(name: &str) -> anyhow::Result<&'static Table> {
    TABLES
        .iter()
        .find(|t| t.name == name)
        .with_context(|| format!("the bundle carries table {name}, which this build does not"))
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn idents(names: &[String]) -> String {
    names
        .iter()
        .map(|n| ident(n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The newest migration `db` has applied; this build must know it.
async fn schema_version(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<String> {
    let applied: Option<String> = sqlx::query_scalar("SELECT max(version) FROM seaql_migrations")
        .fetch_one(&mut **tx)
        .await
        .context("read the database's migrations; start the server once so it migrates")?;
    applied.context("the database records no migration; start the server once so it migrates")
}

/// The table's columns in order, as `COPY` and `INSERT` name them.
async fn columns(tx: &mut Transaction<'_, Postgres>, table: &str) -> anyhow::Result<Vec<String>> {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute \
         WHERE attrelid = to_regclass(quote_ident($1)) AND attnum > 0 AND NOT attisdropped \
         ORDER BY attnum",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await?;
    ensure!(!columns.is_empty(), "the database has no table {table}");
    Ok(columns)
}

/// Write a bundle of `options.selection` to `out`. Nothing is written unless
/// every blob the rows reference is in `store`, so an archive is complete or
/// absent.
pub async fn export(
    db: &DatabaseConnection,
    store: &dyn BlobStore,
    plugins_dir: &Path,
    options: &ExportOptions,
    out: impl Write,
) -> anyhow::Result<Summary> {
    let scope = Scope::new(&options.selection);
    // One connection, checked out through the handle, for `COPY` and the
    // one transaction everything runs in.
    let mut conn = db.acquire().await?;
    let mut tx = sqlx::Connection::begin(conn.raw()).await?;
    // One snapshot for every table and the blob references.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    if let Selection::Contest(id) = options.selection {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM contest WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(exists, "there is no contest {id}");
    }
    let schema = schema_version(&mut tx).await?;
    let installation = installation_id(&mut tx).await?;
    let schemas = config_schemas(&mut tx, store, plugins_dir, &scope).await?;
    let mut withheld = Vec::new();
    let mut config_hashes = BTreeSet::new();
    let mut configs_left_out = Vec::new();

    let mut tables = Vec::new();
    for table in TABLES {
        let Some(rows) = (table.rows)(&scope) else {
            continue;
        };
        let columns: Vec<String> = columns(&mut tx, table.name)
            .await?
            .into_iter()
            .filter(|c| !derived(table.name, c))
            .collect();
        let select: Vec<String> = columns
            .iter()
            .map(|c| match (table.name, c.as_str(), options.with_secrets) {
                ("user", "password", false) => "NULL::text".to_string(),
                _ => ident(c),
            })
            .collect();
        let sql = format!(
            "COPY (SELECT {} FROM {} WHERE {rows}) TO STDOUT",
            select.join(", "),
            ident(table.name)
        );
        let mut spool = tempfile::tempfile()?;
        let mut stream = tx.copy_out_raw(&sql).await?;
        let mut lines = 0u64;
        while let Some(chunk) = stream.try_next().await? {
            lines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
            spool.write_all(&chunk)?;
        }
        drop(stream);
        if table.name == "plugin_config" {
            let written = withhold_secrets(spool, &columns, &schemas)?;
            spool = written.rows;
            lines = written.count;
            withheld = written.withheld;
            config_hashes = written.hashes;
            configs_left_out = written.left_out.into_iter().collect();
        }
        tables.push((
            TableRows {
                name: table.name.to_string(),
                columns,
                rows: lines,
            },
            spool,
        ));
    }

    let mut hashes = referenced_blobs(&mut tx, &scope).await?;
    hashes.extend(config_hashes);
    tx.commit().await?;
    let mut sizes = BTreeMap::new();
    let mut missing = Vec::new();
    for hex in hashes {
        let hash = ContentHash::from_hex(&hex)?;
        match store.size(&hash).await {
            Ok(size) => {
                sizes.insert(hex, (hash, size));
            }
            Err(common::storage::StorageError::NotFound(_)) => missing.push(hex),
            Err(e) => return Err(e).with_context(|| format!("read blob {hex}")),
        }
    }
    if !missing.is_empty() {
        bail!(
            "{} blob(s) the rows reference are missing from storage, so no archive was written: {}",
            missing.len(),
            missing.join(", ")
        );
    }

    let manifest = Manifest {
        format: FORMAT.to_string(),
        schema,
        contest: scope.contest,
        tables: tables.iter().map(|(t, _)| t.clone()).collect(),
        blobs: sizes.keys().cloned().collect(),
        withheld,
        installation,
    };
    let gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut archive = tar::Builder::new(gz);
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    append(
        &mut archive,
        MANIFEST,
        manifest_json.len() as u64,
        &manifest_json[..],
    )?;
    let mut summary = Summary {
        configs_left_out,
        ..Summary::default()
    };
    for (rows, mut spool) in tables {
        let len = spool.stream_position()?;
        spool.rewind()?;
        append(
            &mut archive,
            &format!("{DB_DIR}{}.copy", rows.name),
            len,
            spool,
        )?;
        summary.tables.push((rows.name, rows.rows));
    }
    for (name, (hash, size)) in &sizes {
        let reader = store.get_stream(hash).await?;
        // tar reads synchronously; the bridge drives the blob stream on the
        // runtime from this blocking section.
        tokio::task::block_in_place(|| {
            append(
                &mut archive,
                &format!("{BLOB_DIR}{name}"),
                *size,
                SyncIoBridge::new(reader),
            )
        })?;
    }
    summary.blobs = sizes.len();
    archive.into_inner()?.finish()?.flush()?;
    Ok(summary)
}

fn append<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    data: impl Read,
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, path, data)
        .with_context(|| format!("write {path} to the archive"))
}

/// Every blob the bundle's table rows reference: test case bodies,
/// attachments and additional files. The hashes plugin config holds come
/// from the configs as the bundle writes them ([`withhold_secrets`]), so a
/// withheld secret is never read as one.
async fn referenced_blobs(
    tx: &mut Transaction<'_, Postgres>,
    scope: &Scope,
) -> anyhow::Result<BTreeSet<String>> {
    let p = &scope.problems;
    let sql = format!(
        "SELECT input_blob_hash FROM test_case WHERE problem_id IN ({p}) \
         UNION SELECT expected_output_blob_hash FROM test_case WHERE problem_id IN ({p}) \
         UNION SELECT content_hash FROM problem_attachment WHERE problem_id IN ({p}) \
         UNION SELECT content_hash FROM additional_file WHERE problem_id IN ({p})"
    );
    let hashes: Vec<Option<String>> = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(&mut **tx)
        .await?;
    Ok(hashes
        .into_iter()
        .flatten()
        .filter(|h| ContentHash::from_hex(h).is_ok())
        .collect())
}

/// The config schemas of every plugin the bundle's configs belong to, by
/// plugin id then namespace, from the plugin's published upload in `store`
/// or its copy in `plugins_dir`, as the server would load it. The export
/// runs no plugin, and withholds secrets by these schemas. A plugin with no
/// manifest at hand (`None`: removed, or never installed here) is one no one
/// could say the secret fields of, and whose configs nothing here reads.
async fn config_schemas(
    tx: &mut Transaction<'_, Postgres>,
    store: &dyn BlobStore,
    plugins_dir: &Path,
    scope: &Scope,
) -> anyhow::Result<BTreeMap<String, Option<BTreeMap<String, ConfigNamespace>>>> {
    let Some(rows) = table("plugin_config").ok().and_then(|t| (t.rows)(scope)) else {
        return Ok(BTreeMap::new());
    };
    let plugins: Vec<(String, Option<String>)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT ids.id, plugin.archive_hash FROM \
           (SELECT DISTINCT split_part(namespace, ':', 1) AS id FROM plugin_config WHERE {rows}) ids \
         LEFT JOIN plugin ON plugin.id = ids.id"
    )))
    .fetch_all(&mut **tx)
    .await?;
    let mut schemas = BTreeMap::new();
    for (id, archive) in plugins {
        let config = match archive {
            Some(hex) => {
                let hash = ContentHash::from_hex(&hex)
                    .with_context(|| format!("plugin {id}'s published archive hash"))?;
                let bytes = store
                    .get(&hash)
                    .await
                    .with_context(|| format!("read plugin {id}'s published archive"))?;
                PluginArchive::parse(bytes)
                    .with_context(|| format!("read plugin {id}'s published archive"))?
                    .manifest()
                    .config
                    .clone()
            }
            None => {
                let root = plugins_dir.join(&id);
                if !root.join("plugin.toml").is_file() {
                    schemas.insert(id, None);
                    continue;
                }
                plugin_core::registry::PluginEntry::from_dir(&root)
                    .with_context(|| format!("read plugin {id}'s manifest"))?
                    .manifest
                    .config
            }
        };
        schemas.insert(id, Some(config.into_iter().collect()));
    }
    Ok(schemas)
}

/// The `plugin_config` rows of an export, as the bundle carries them.
struct Withholding {
    rows: std::fs::File,
    count: u64,
    withheld: Vec<Withheld>,
    /// The plugins, and `plugin:namespace` rows, left out.
    left_out: BTreeSet<String>,
    /// The blob hashes the configs hold.
    hashes: BTreeSet<String>,
}

/// `rows` (`plugin_config` in `COPY` text format, with `columns`) with each
/// secret field its namespace declares withheld
/// ([`ConfigNamespace::withhold_secrets`]), and where, leaving out the rows
/// of a plugin whose schemas are unknown and of a namespace its manifest
/// does not declare.
fn withhold_secrets(
    mut rows: std::fs::File,
    columns: &[String],
    schemas: &BTreeMap<String, Option<BTreeMap<String, ConfigNamespace>>>,
) -> anyhow::Result<Withholding> {
    let at = |name: &str| {
        columns
            .iter()
            .position(|c| c == name)
            .with_context(|| format!("plugin_config has no {name} column"))
    };
    let (scope, ref_id, namespace, config) =
        (at("scope")?, at("ref_id")?, at("namespace")?, at("config")?);
    rows.rewind()?;
    let mut out = tempfile::tempfile()?;
    let mut withheld = Vec::new();
    let mut hashes = BTreeSet::new();
    let mut left_out = BTreeSet::new();
    let mut count = 0;
    for line in BufReader::new(rows).lines() {
        let line = line?;
        let mut fields: Vec<String> = line.split('\t').map(str::to_string).collect();
        ensure!(
            fields.len() == columns.len(),
            "a plugin_config row has the wrong field count"
        );
        let stored = copy_text::decode(&fields[namespace])?;
        let mut value: serde_json::Value =
            serde_json::from_str(&copy_text::decode(&fields[config])?)
                .context("a plugin_config row's config is not JSON")?;
        let plugin = extract_plugin_id(&stored);
        let Some(Some(namespaces)) = schemas.get(plugin) else {
            left_out.insert(plugin.to_string());
            continue;
        };
        // A namespace the manifest no longer declares has no schema to say
        // which of its fields are secret: the row stays out like the
        // configs of an unknown plugin.
        let Some(ns) = namespaces.get(strip_namespace_prefix(&stored)) else {
            left_out.insert(stored);
            continue;
        };
        let secrets: Vec<_> = ns
            .withhold_secrets(&mut value)
            .into_iter()
            .map(|set| set.identity)
            .collect();
        if !secrets.is_empty() {
            withheld.push(Withheld {
                scope: copy_text::decode(&fields[scope])?,
                ref_id: copy_text::decode(&fields[ref_id])?,
                namespace: stored,
                secrets,
            });
        }
        let text = value.to_string();
        hashes.extend(content_hashes(&text));
        fields[config] = copy_text::encode(&text);
        writeln!(out, "{}", fields.join("\t"))?;
        count += 1;
    }
    Ok(Withholding {
        rows: out,
        count,
        withheld,
        left_out,
        hashes,
    })
}

/// Each run of 64 lowercase hex digits in `text`, left to right, as the
/// content hashes a config may name.
fn content_hashes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    let mut found = Vec::new();
    let mut i = 0;
    while i + 64 <= bytes.len() {
        if bytes[i..i + 64].iter().all(hex) {
            found.push(text[i..i + 64].to_string());
            i += 64;
        } else {
            i += 1;
        }
    }
    found
}

/// The field escapes of Postgres `COPY` text format.
mod copy_text {
    /// A field as `COPY ... TO` writes it, decoded. A `NULL` is refused: no
    /// field this module reads may be one.
    pub(super) fn decode(field: &str) -> anyhow::Result<String> {
        anyhow::ensure!(field != "\\N", "an unexpected NULL field");
        let mut out = String::with_capacity(field.len());
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next() {
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('b') => '\u{8}',
                Some('f') => '\u{c}',
                Some('v') => '\u{b}',
                other => anyhow::bail!("an unknown COPY escape \\{other:?}"),
            });
        }
        Ok(out)
    }

    /// `text` as a `COPY ... FROM` field.
    pub(super) fn encode(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\u{b}' => out.push_str("\\v"),
                c => out.push(c),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_field_round_trips_every_escape() {
            let text = "a\\b\nc\rd\te\u{8}f\u{c}g\u{b}h \"q\"";
            assert_eq!(decode(&encode(text)).unwrap(), text);
            assert_eq!(encode("x\ty"), "x\\ty");
        }
    }
}

/// This database's installation id (`entity::installation`).
async fn installation_id(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<String> {
    sqlx::query_scalar("SELECT installation_id FROM installation WHERE id = $1")
        .bind(crate::entity::installation::ROW_ID)
        .fetch_one(&mut **tx)
        .await
        .context("read the installation id")
}

/// The configs the target stores for the rows whose secrets the export
/// withheld, read before the import replaces anything: replacing a problem
/// deletes its configs, and the secrets they hold are what the import keeps.
async fn stored_configs(
    tx: &mut Transaction<'_, Postgres>,
    withheld: &[Withheld],
) -> anyhow::Result<BTreeMap<(String, String, String), serde_json::Value>> {
    let mut stored = BTreeMap::new();
    for row in withheld {
        let config: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT config FROM plugin_config WHERE scope = $1 AND ref_id = $2 AND namespace = $3",
        )
        .bind(&row.scope)
        .bind(&row.ref_id)
        .bind(&row.namespace)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(config) = config {
            stored.insert(
                (row.scope.clone(), row.ref_id.clone(), row.namespace.clone()),
                config,
            );
        }
    }
    Ok(stored)
}

/// Keeps, in the staged `plugin_config` rows, each secret the export
/// withheld: the value the target stored for the same item of the same
/// config before the import (`stored`), else nothing, so the field reads
/// unset until someone sets it.
async fn keep_withheld_secrets(
    tx: &mut Transaction<'_, Postgres>,
    withheld: &[Withheld],
    stored: &BTreeMap<(String, String, String), serde_json::Value>,
) -> anyhow::Result<()> {
    let staged = staging("plugin_config");
    for row in withheld {
        let found: Option<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT config FROM {staged} WHERE scope = $1 AND ref_id = $2 AND namespace = $3"
        )))
        .bind(&row.scope)
        .bind(&row.ref_id)
        .bind(&row.namespace)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(mut config) = found else {
            continue;
        };
        let key = (row.scope.clone(), row.ref_id.clone(), row.namespace.clone());
        keep_withheld(&mut config, stored.get(&key), &row.secrets);
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {staged} SET config = $4 \
             WHERE scope = $1 AND ref_id = $2 AND namespace = $3"
        )))
        .bind(&row.scope)
        .bind(&row.ref_id)
        .bind(&row.namespace)
        .bind(&config)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Restore the bundle in `input` onto the database and `store`. The rows go
/// in one transaction, committed after every blob is stored, so a failed
/// import changes no row.
pub async fn import(
    db: &DatabaseConnection,
    store: Arc<dyn BlobStore>,
    options: ImportOptions,
    input: impl Read,
) -> anyhow::Result<Summary> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(input));
    let mut entries = archive.entries()?;
    let mut first = entries.next().context("the archive is empty")??;
    ensure!(
        first.path()?.to_str() == Some(MANIFEST),
        "not a Broccoli bundle: it does not start with {MANIFEST}"
    );
    let mut manifest_json = Vec::new();
    first.read_to_end(&mut manifest_json)?;
    drop(first);
    let manifest: Manifest = serde_json::from_slice(&manifest_json).context("read the manifest")?;
    ensure!(
        manifest.format == FORMAT,
        "the bundle's format is {}, this build reads {FORMAT}",
        manifest.format
    );
    let known = known_migrations::<Migrator>();
    ensure!(
        known.contains(&manifest.schema),
        "the bundle comes from a database at schema {}, which this build does not know; \
         import it with a build at least as new",
        manifest.schema
    );

    ensure!(
        !options.wipe_dependents || options.replace_problems || options.replace_contest,
        "wiping dependents needs --replace-problems or --replace-contest"
    );
    // One connection, checked out through the handle, for `COPY` and the
    // one transaction everything runs in.
    let mut conn = db.acquire().await?;
    let mut tx = sqlx::Connection::begin(conn.raw()).await?;
    // The rows are restored as they were: no inputs version counts them.
    sqlx::query(crate::services::judging::RESTORING)
        .execute(&mut *tx)
        .await?;
    let target = schema_version(&mut tx).await?;
    ensure!(
        known.last() == Some(&target),
        "the database is at schema {target}, this build at {}; start this build's server once \
         so it migrates the database",
        known.last().map(String::as_str).unwrap_or_default()
    );
    // Only this installation's own export may keep what it stores: another
    // installation's secret for a config of the same id was never the
    // bundle's.
    let own = manifest.installation == installation_id(&mut tx).await?;
    let stored = if own {
        stored_configs(&mut tx, &manifest.withheld).await?
    } else {
        BTreeMap::new()
    };
    prepare_problem_tables(&mut tx, options).await?;

    let mut summary = Summary {
        secrets_unset: if own {
            0
        } else {
            manifest.withheld.iter().map(|row| row.secrets.len()).sum()
        },
        ..Summary::default()
    };
    let mut loaded = Vec::new();
    for entry in entries {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        if let Some(file) = path.strip_prefix(DB_DIR) {
            let name = file.strip_suffix(".copy").unwrap_or(file);
            let rows = manifest
                .tables
                .iter()
                .find(|t| t.name == name)
                .with_context(|| format!("{path} is not in the manifest"))?;
            let table = table(name)?;
            load(&mut tx, table, &rows.columns, &mut entry).await?;
            loaded.push((table, &rows.columns));
            summary.tables.push((rows.name.clone(), rows.rows));
        } else if let Some(name) = path.strip_prefix(BLOB_DIR) {
            let expected = ContentHash::from_hex(name)
                .with_context(|| format!("{path} is not named by a content hash"))?;
            let stored = put_blob(&store, &mut entry).await?;
            ensure!(
                stored == expected,
                "{path} holds content whose hash is {stored}; the archive is corrupt"
            );
            summary.blobs += 1;
        } else {
            bail!("unexpected archive entry {path}");
        }
    }
    let missing: Vec<&TableRows> = manifest
        .tables
        .iter()
        .filter(|t| !loaded.iter().any(|(table, _)| table.name == t.name))
        .collect();
    ensure!(
        missing.is_empty(),
        "the archive lacks the rows of {:?}; it is truncated",
        missing.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    ensure!(
        summary.blobs == manifest.blobs.len(),
        "the archive holds {} of the {} blobs its manifest lists; it is truncated",
        summary.blobs,
        manifest.blobs.len()
    );

    if loaded
        .iter()
        .any(|(table, _)| table.name == "plugin_config")
    {
        keep_withheld_secrets(&mut tx, &manifest.withheld, &stored).await?;
    }
    if loaded.iter().any(|(table, _)| table.name == "contest") {
        prepare_contest(&mut tx, options).await?;
    }

    // Restore in table order, whatever order the archive held them in.
    loaded.sort_by_key(|(table, _)| TABLES.iter().position(|t| t.name == table.name));
    for (table, columns) in &loaded {
        let columns: Vec<String> = columns
            .iter()
            .filter(|c| !derived(table.name, c))
            .cloned()
            .collect();
        for statement in restore_statements(table, &columns) {
            sqlx::query(AssertSqlSafe(statement.as_str()))
                .execute(&mut *tx)
                .await
                .with_context(|| format!("restore {}", table.name))?;
        }
        // New rows on the target must not collide with the restored ids.
        if columns.iter().any(|c| c == "id") {
            let t = ident(table.name);
            let sequence: Option<String> =
                sqlx::query_scalar("SELECT pg_get_serial_sequence($1, 'id')")
                    .bind(&t)
                    .fetch_one(&mut *tx)
                    .await?;
            if let Some(sequence) = sequence {
                sqlx::query(AssertSqlSafe(format!(
                    "SELECT setval($1::regclass, GREATEST((SELECT COALESCE(max(id), 1) FROM {t}), 1))"
                )))
                .bind(sequence)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    let restored: Vec<&str> = loaded.iter().map(|(table, _)| table.name).collect();
    mark_for_recheck(&mut tx, &restored).await?;
    tx.commit().await?;
    Ok(summary)
}

/// Marks every problem the import restored, or restored a contest's use of,
/// for a config issues recheck any replica can make: the import runs no
/// plugin, so it checks none of the configs and test data it restores. Until
/// a running server decides it on its next reconcile pass
/// (`services::config_issues::recheck_marked`), the problem's issues are
/// unknown, which reads as having issues: judgements claimed meanwhile say
/// so.
async fn mark_for_recheck(
    tx: &mut Transaction<'_, Postgres>,
    restored: &[&str],
) -> anyhow::Result<()> {
    let of: Vec<String> = [("problem", "id"), ("contest_problem", "problem_id")]
        .into_iter()
        .filter(|(table, _)| restored.contains(table))
        .map(|(table, column)| format!("SELECT {} FROM {}", ident(column), staging(table)))
        .collect();
    if of.is_empty() {
        return Ok(());
    }
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE problem SET config_issues = true, config_recheck = '[]'::jsonb \
         WHERE id IN ({})",
        of.join(" UNION ")
    )))
    .execute(&mut **tx)
    .await
    .context("mark the restored problems for a config issues recheck")?;
    Ok(())
}

/// Empty the problem tables for the bundle's problems. Refuse to import onto
/// problems unless asked to replace them, and refuse to replace problems
/// anything references unless asked to delete that too. The referencing
/// tables are read from the schema's foreign keys.
async fn prepare_problem_tables(
    tx: &mut Transaction<'_, Postgres>,
    options: ImportOptions,
) -> anyhow::Result<()> {
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM problem")
        .fetch_one(&mut **tx)
        .await?;
    if existing > 0 {
        replace_problems(tx, options, existing).await?;
    }
    // The bundle's problems keep the ids they had on the source. A config
    // names its problem by id in text, with no foreign key, so every problem
    // and contest-problem config the target holds goes with the problems it
    // had: none stays to configure the imported problem that takes its id.
    sqlx::query("DELETE FROM plugin_config WHERE scope = ANY($1)")
        .bind([
            ConfigScope::Problem.as_str(),
            ConfigScope::ContestProblem.as_str(),
        ])
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Clear the target's contest that has the bundle's contest id. A contest
/// bundle restores the contest as it was exported, so a target contest
/// holding that id is replaced, never merged with: refused unless asked to
/// replace it. Then its roster, problem links and contest-scope configs go,
/// and so does every other row that references it (its clarifications,
/// submissions, code runs), which is refused unless asked to delete that
/// too. The contest row itself is overwritten by the bundle's.
async fn prepare_contest(
    tx: &mut Transaction<'_, Postgres>,
    options: ImportOptions,
) -> anyhow::Result<()> {
    let Some(contest): Option<i32> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT b.id FROM {} b JOIN contest c ON c.id = b.id",
        staging("contest")
    )))
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(());
    };
    ensure!(
        options.replace_contest,
        "the database already has contest {contest}; pass --replace-contest to replace it with \
         the bundle's"
    );
    // What the bundle restores for the contest; anything else is a dependent.
    const RESTORED: [&str; 2] = ["contest_problem", "contest_user"];
    let mut counts = Vec::new();
    for (table, column, _) in foreign_keys_to(tx, "contest").await? {
        if RESTORED.contains(&table.as_str()) {
            continue;
        }
        let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE {} = $1",
            ident(&column)
        )))
        .bind(contest)
        .fetch_one(&mut **tx)
        .await?;
        if n > 0 {
            counts.push(format!("{table}={n}"));
        }
    }
    ensure!(
        counts.is_empty() || options.wipe_dependents,
        "rows reference contest {contest} ({}); pass --wipe-dependents to delete them too",
        counts.join(", ")
    );
    for (table, column, _) in foreign_keys_to(tx, "contest").await? {
        delete_with_dependents(
            tx,
            &table,
            format!("{} = {contest}", ident(&column)),
            &mut Vec::new(),
        )
        .await?;
    }
    sqlx::query("DELETE FROM plugin_config WHERE scope = $1 AND ref_id = $2")
        .bind(ConfigScope::Contest.as_str())
        .bind(contest.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The single-column foreign keys that reference `table` (a name as
/// `regclass` prints it): each referencing table, as `regclass` prints it,
/// its column, and the referenced column.
async fn foreign_keys_to(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
) -> anyhow::Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT c.conrelid::regclass::text, a.attname::text, pa.attname::text \
         FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1] \
         JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = c.confkey[1] \
         WHERE c.contype = 'f' AND c.confrelid = $1::regclass AND cardinality(c.conkey) = 1 \
         ORDER BY 1, 2",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await?)
}

/// Delete the rows of `table` that `filter` selects, after every row that
/// references them, however deep. `path` holds the tables being deleted
/// from above this one, so a self-reference ends the walk.
fn delete_with_dependents<'a>(
    tx: &'a mut Transaction<'_, Postgres>,
    table: &'a str,
    filter: String,
    path: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        path.push(table.to_string());
        for (child, column, parent) in foreign_keys_to(tx, table).await? {
            if path.contains(&child) {
                continue;
            }
            let rows = format!(
                "{} IN (SELECT {} FROM {table} WHERE {filter})",
                ident(&column),
                ident(&parent)
            );
            delete_with_dependents(tx, &child, rows, path).await?;
        }
        path.pop();
        sqlx::query(AssertSqlSafe(format!("DELETE FROM {table} WHERE {filter}")))
            .execute(&mut **tx)
            .await?;
        Ok(())
    })
}

/// Delete the target's `existing` problems and, with `wipe_dependents`,
/// every row that references them.
async fn replace_problems(
    tx: &mut Transaction<'_, Postgres>,
    options: ImportOptions,
    existing: i64,
) -> anyhow::Result<()> {
    ensure!(
        options.replace_problems,
        "the database already has {existing} problem(s); pass --replace-problems to replace them, \
         which removes every problem it has, those the bundle does not carry included"
    );
    let replaced: Vec<String> = problem_tables().map(str::to_string).collect();
    // Every table whose rows reference a replaced row, directly or through
    // another such table.
    let referencing: Vec<String> = sqlx::query_scalar(
        "WITH RECURSIVE refs(t) AS ( \
           SELECT unnest($1::text[])::regclass \
           UNION SELECT c.conrelid::regclass FROM pg_constraint c JOIN refs ON c.confrelid = refs.t \
           WHERE c.contype = 'f') \
         SELECT t::text FROM refs",
    )
    .bind(&replaced)
    .fetch_all(&mut **tx)
    .await?;
    let dependents: Vec<&String> = referencing
        .iter()
        .filter(|t| !replaced.contains(t))
        .collect();
    let mut counts = Vec::new();
    for table in &dependents {
        let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&mut **tx)
            .await?;
        if n > 0 {
            counts.push(format!("{table}={n}"));
        }
    }
    ensure!(
        counts.is_empty() || options.wipe_dependents,
        "rows reference the problems being replaced ({}); pass --wipe-dependents to delete them too",
        counts.join(", ")
    );
    sqlx::query(AssertSqlSafe(format!(
        "TRUNCATE {} RESTART IDENTITY",
        referencing.join(", ")
    )))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn staging(table: &str) -> String {
    ident(&format!("bundle_{table}"))
}

/// `COPY` one table's rows from the archive into its staging table.
async fn load(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    columns: &[String],
    rows: &mut impl Read,
) -> anyhow::Result<()> {
    let staging = staging(table.name);
    sqlx::query(AssertSqlSafe(format!(
        "CREATE TEMP TABLE {staging} (LIKE {} INCLUDING DEFAULTS) ON COMMIT DROP",
        ident(table.name)
    )))
    .execute(&mut **tx)
    .await?;
    let mut copy = tx
        .copy_in_raw(&format!("COPY {staging} ({}) FROM STDIN", idents(columns)))
        .await
        .with_context(|| format!("load {}", table.name))?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = rows.read(&mut buf)?;
        if n == 0 {
            break;
        }
        copy.send(&buf[..n]).await?;
    }
    copy.finish()
        .await
        .with_context(|| format!("load {}", table.name))?;
    Ok(())
}

/// The statements that move one staged table into the real one.
fn restore_statements(table: &Table, columns: &[String]) -> Vec<String> {
    let t = ident(table.name);
    let s = staging(table.name);
    let all = idents(columns);
    let merge = |key: &[&str], update: bool| {
        let rest: Vec<String> = columns
            .iter()
            .filter(|c| !key.contains(&c.as_str()))
            .map(|c| format!("{0} = EXCLUDED.{0}", ident(c)))
            .collect();
        let key = key.iter().map(|k| ident(k)).collect::<Vec<_>>().join(", ");
        if update && !rest.is_empty() {
            format!("ON CONFLICT ({key}) DO UPDATE SET {}", rest.join(", "))
        } else {
            format!("ON CONFLICT ({key}) DO NOTHING")
        }
    };
    match &table.restore {
        Restore::Insert => vec![format!("INSERT INTO {t} ({all}) SELECT {all} FROM {s}")],
        Restore::Merge { key, update } => vec![format!(
            "INSERT INTO {t} ({all}) SELECT {all} FROM {s} {}",
            merge(key, *update)
        )],
        Restore::NewRoles => vec![
            format!(
                "CREATE TEMP TABLE bundle_new_roles ON COMMIT DROP AS \
                 SELECT name FROM {s} WHERE name NOT IN (SELECT name FROM {t})"
            ),
            format!(
                "INSERT INTO {t} ({all}) SELECT {all} FROM {s} \
                 WHERE name IN (SELECT name FROM bundle_new_roles)"
            ),
        ],
        Restore::NewRolePermissions => vec![format!(
            "INSERT INTO {t} ({all}) SELECT {all} FROM {s} \
             WHERE role IN (SELECT name FROM bundle_new_roles) ON CONFLICT DO NOTHING"
        )],
        Restore::Accounts => {
            let new: Vec<String> = columns.iter().filter(|c| *c != "id").cloned().collect();
            let new = idents(&new);
            let same = format!(
                "{} = {}",
                username_key_sql("u.username"),
                username_key_sql("b.username")
            );
            // The accounts the target has are mapped before the new ones are
            // inserted, so `created` tells the two apart.
            vec![
                format!(
                    "CREATE TEMP TABLE bundle_account_ids ON COMMIT DROP AS \
                     SELECT b.id AS bundle_id, u.id AS target_id, false AS created FROM {s} b \
                     JOIN {t} u ON {same} AND u.deleted_at IS NULL"
                ),
                format!(
                    "WITH created AS (\
                       INSERT INTO {t} ({new}) SELECT {new} FROM {s} b \
                       WHERE b.id NOT IN (SELECT bundle_id FROM bundle_account_ids) \
                       RETURNING id, username) \
                     INSERT INTO bundle_account_ids (bundle_id, target_id, created) \
                     SELECT b.id, u.id, true FROM created u JOIN {s} b ON {same}"
                ),
            ]
        }
        Restore::AccountRefs { key, of } => {
            let select: Vec<String> = columns
                .iter()
                .map(|c| {
                    if c == "user_id" {
                        "a.target_id".to_string()
                    } else {
                        format!("r.{}", ident(c))
                    }
                })
                .collect();
            let accounts = match of {
                AccountsOf::Created => " AND a.created",
                AccountsOf::Active => "",
            };
            vec![format!(
                "INSERT INTO {t} ({all}) SELECT {} FROM {s} r \
                 JOIN bundle_account_ids a ON a.bundle_id = r.user_id{accounts} {}",
                select.join(", "),
                merge(key, false)
            )]
        }
    }
}

/// Store one archive entry as a blob, streaming it.
async fn put_blob(store: &Arc<dyn BlobStore>, data: &mut impl Read) -> anyhow::Result<ContentHash> {
    let (mut tx, rx) = tokio::io::duplex(256 * 1024);
    let store = store.clone();
    let put = tokio::spawn(async move { store.put_stream(Box::new(rx)).await });
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = data.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if tx.write_all(&buf[..n]).await.is_err() {
            // The store stopped reading; its error is the one to report.
            break;
        }
    }
    drop(tx);
    Ok(put.await.context("store a blob")??)
}
