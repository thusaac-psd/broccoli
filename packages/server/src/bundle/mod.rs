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
//! Columns are read from the database at export and named at import, so a
//! bundle restores onto any build that knows its schema version: a column
//! added since then takes its default.

pub mod cli;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};
use std::sync::Arc;

use anyhow::{Context, bail, ensure};
use common::db::DatabaseConnection;
use common::storage::{BlobStore, ContentHash};
use futures::TryStreamExt;
use sea_orm::sqlx::{self, AssertSqlSafe, Postgres, Transaction};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::SyncIoBridge;

use crate::migration::{Migrator, known_migrations};

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
    /// Carry the accounts' password hashes. Without it every password is
    /// blank, so the archive holds no credentials.
    pub with_secrets: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// Replace the problems the target already has. Refused while anything
    /// references them, unless `wipe_dependents`.
    pub replace_problems: bool,
    /// With `replace_problems`, also delete every row that references the
    /// replaced problems (submissions and their results, code runs, contest
    /// problem links).
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
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (table, rows) in &self.tables {
            writeln!(f, "{table}: {rows} rows")?;
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
    /// Accounts merge by username. An active account the target has stays as
    /// it is, password included; a new one gets a new id. Every bundle row that
    /// references an account follows it to its id on the target.
    Accounts,
    /// Rows that reference an account (`user_id`), moved to the account's id
    /// on the target, then merged on `key`.
    AccountRefs { key: &'static [&'static str] },
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
    // Plugin-wide config, and the config of the bundle's problems, contest
    // and contest problems (`models::plugin_config::config_key` names them).
    Table {
        name: "plugin_config",
        rows: |s| {
            let mut rows = format!(
                "scope = 'plugin' OR (scope = 'problem' AND ref_id IN \
                 (SELECT id::text FROM ({}) p))",
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
        },
    },
];

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

/// The database's sqlx pool, for `COPY` and one-connection transactions.
fn pool(db: &DatabaseConnection) -> &sqlx::PgPool {
    db.as_sea_orm().get_postgres_connection_pool()
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
    options: &ExportOptions,
    out: impl Write,
) -> anyhow::Result<Summary> {
    let scope = Scope::new(&options.selection);
    let mut tx = pool(db).begin().await?;
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

    let mut tables = Vec::new();
    for table in TABLES {
        let Some(rows) = (table.rows)(&scope) else {
            continue;
        };
        let columns = columns(&mut tx, table.name).await?;
        let select: Vec<String> = columns
            .iter()
            .map(|c| match (table.name, c.as_str(), options.with_secrets) {
                ("user", "password", false) => "''::text".to_string(),
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
        tables.push((
            TableRows {
                name: table.name.to_string(),
                columns,
                rows: lines,
            },
            spool,
        ));
    }

    let hashes = referenced_blobs(&mut tx, &scope).await?;
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
    let mut summary = Summary::default();
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

/// Every blob the bundle's rows reference: test case bodies, attachments,
/// additional files, and the hashes plugin config holds.
async fn referenced_blobs(
    tx: &mut Transaction<'_, Postgres>,
    scope: &Scope,
) -> anyhow::Result<BTreeSet<String>> {
    let config = TABLES
        .iter()
        .find(|t| t.name == "plugin_config")
        .and_then(|t| (t.rows)(scope))
        .unwrap_or_else(|| "false".into());
    let p = &scope.problems;
    let sql = format!(
        "SELECT input_blob_hash FROM test_case WHERE problem_id IN ({p}) \
         UNION SELECT expected_output_blob_hash FROM test_case WHERE problem_id IN ({p}) \
         UNION SELECT content_hash FROM problem_attachment WHERE problem_id IN ({p}) \
         UNION SELECT content_hash FROM additional_file WHERE problem_id IN ({p}) \
         UNION SELECT (regexp_matches(config::text, '[0-9a-f]{{64}}', 'g'))[1] \
           FROM plugin_config WHERE {config}"
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

    let mut tx = pool(db).begin().await?;
    let target = schema_version(&mut tx).await?;
    ensure!(
        known.last() == Some(&target),
        "the database is at schema {target}, this build at {}; start this build's server once \
         so it migrates the database",
        known.last().map(String::as_str).unwrap_or_default()
    );
    prepare_problem_tables(&mut tx, options).await?;

    let mut summary = Summary::default();
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

    // Restore in table order, whatever order the archive held them in.
    loaded.sort_by_key(|(table, _)| TABLES.iter().position(|t| t.name == table.name));
    for (table, columns) in &loaded {
        for statement in restore_statements(table, columns) {
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
    tx.commit().await?;
    Ok(summary)
}

/// Refuse to import onto problems unless asked to replace them, and refuse to
/// replace problems anything references unless asked to delete that too. The
/// referencing tables are read from the schema's foreign keys.
async fn prepare_problem_tables(
    tx: &mut Transaction<'_, Postgres>,
    options: ImportOptions,
) -> anyhow::Result<()> {
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM problem")
        .fetch_one(&mut **tx)
        .await?;
    if existing == 0 {
        return Ok(());
    }
    ensure!(
        options.replace_problems,
        "the database already has {existing} problem(s); pass --replace-problems to replace them"
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
            vec![
                format!(
                    "INSERT INTO {t} ({new}) SELECT {new} FROM {s} b WHERE NOT EXISTS \
                     (SELECT 1 FROM {t} u WHERE u.username = b.username AND u.deleted_at IS NULL)"
                ),
                format!(
                    "CREATE TEMP TABLE bundle_account_ids ON COMMIT DROP AS \
                     SELECT b.id AS bundle_id, u.id AS target_id FROM {s} b \
                     JOIN {t} u ON u.username = b.username AND u.deleted_at IS NULL"
                ),
            ]
        }
        Restore::AccountRefs { key } => {
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
            vec![format!(
                "INSERT INTO {t} ({all}) SELECT {} FROM {s} r \
                 JOIN bundle_account_ids a ON a.bundle_id = r.user_id {}",
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
