//! A plugin's database identity: the login role its SQL runs as, the schema
//! that role owns, and the DDL that provisions both.
//!
//! Every name is derived from the plugin id alone, so the host holds no
//! per-plugin knowledge. The role is a member of the database's plugin group
//! role ([`PluginDbNames::group`]), which carries the read grants on host
//! relations (migration m0001), and owns
//! schema `plugin_<id>`, where it has full DDL and DML. It has no other
//! privilege: it cannot write host state, read secrets, or reach another
//! plugin's schema.

use common::db::DatabaseConnection;
use plugin_core::manifest::{SQL_READ_SCOPES, SqlReadScope};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement, TransactionTrait};
use sha2::{Digest, Sha256};

/// The table holding each plugin role's login password (migration m0001). No
/// role but the app role can read it.
const SECRET_TABLE: &str = "plugin_login_secret";

/// Postgres truncates identifiers beyond this many bytes.
const MAX_IDENT_BYTES: usize = 63;

/// `prefix` followed by `id`, fitted to [`MAX_IDENT_BYTES`]: an id too long to
/// fit keeps its head and ends in a digest of the whole id, so distinct ids
/// keep distinct names.
fn bounded_ident(prefix: &str, id: &str) -> String {
    if prefix.len() + id.len() <= MAX_IDENT_BYTES {
        return format!("{prefix}{id}");
    }
    let digest = hex::encode(&Sha256::digest(id.as_bytes())[..8]);
    let mut keep = MAX_IDENT_BYTES - prefix.len() - 1 - digest.len();
    while !id.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{prefix}{}_{digest}", &id[..keep])
}

/// `name` as a quoted SQL identifier.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` as a SQL string literal.
fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The schema and the roles of one plugin in one database.
///
/// Roles are shared by every database of a cluster, so role names carry a
/// digest of the database name: two databases on one cluster never share a
/// plugin role or a group role, and each database's app role created, and so
/// administers, its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDbNames {
    /// `plugin_<id>`: the schema the role owns, first on its `search_path`.
    pub schema: String,
    /// The login role, `broccoli_<db>_plugin_<id>`.
    pub role: String,
    /// The database's plugin group role (see [`group_role`]).
    pub group: String,
}

impl PluginDbNames {
    /// The names of `plugin_id` in `database`, whose group role is `group`.
    pub fn new(database: &str, group: &str, plugin_id: &str) -> Self {
        Self {
            schema: bounded_ident("plugin_", plugin_id),
            role: bounded_ident(&format!("{}_", group_role(database)), plugin_id),
            group: group.to_string(),
        }
    }
}

/// The NOLOGIN role, `broccoli_<db>_plugin`, every plugin role of a database
/// belongs to, named when the database is migrated and recorded in
/// [`GROUP_TABLE`]. It holds `USAGE` on schema `public` and the read grants on
/// host relations, and nothing else. A plugin role's name extends it, so no
/// plugin id names a group.
pub fn group_role(database: &str) -> String {
    let database_tag = hex::encode(&Sha256::digest(database.as_bytes())[..4]);
    format!("broccoli_{database_tag}_plugin")
}

/// The one-row table naming the database's plugin group role (migration
/// m0001). A database cloned from a template keeps its template's group and
/// the grants made to it.
pub const GROUP_TABLE: &str = "plugin_group_role";

/// The content columns of host `table`: the ones a plugin reads only by
/// declaring them ([`SQL_READ_SCOPES`]).
pub fn content_columns(table: &str) -> Vec<&'static str> {
    SQL_READ_SCOPES
        .iter()
        .filter_map(|scope| scope.split_once('.'))
        .filter(|(t, _)| *t == table)
        .map(|(_, column)| column)
        .collect()
}

/// What a plugin's SQL pool connects with.
#[derive(Clone)]
pub struct PluginLogin {
    pub names: PluginDbNames,
    pub password: String,
}

/// Creates or repairs `plugin_id`'s role and schema, on the app role's
/// connection `host`, and returns the login the plugin's pool connects with.
///
/// Idempotent, and safe on replicas provisioning the same plugin at once: the
/// statements run in one transaction serialized on the plugin's row in
/// `plugin_login_secret`, which no plugin can lock. The first writer's password
/// wins and every replica reads it back, so all replicas connect with the
/// same password.
///
/// The role's `search_path` is its schema followed by the app's own
/// `search_path`, so unqualified DDL lands in the plugin's schema and
/// unqualified reads find the host relations.
pub async fn provision(
    host: &DatabaseConnection,
    plugin_id: &str,
    read: &[SqlReadScope],
) -> Result<PluginLogin, DbErr> {
    let txn = host.begin().await?;
    let row = txn
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT current_database()::text AS database, \
                        current_setting('search_path') AS path, \
                        (SELECT name FROM {GROUP_TABLE}) AS grp"
            ),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("current_database() returned no row".into()))?;
    let database: String = row.try_get("", "database")?;
    let path: String = row.try_get("", "path")?;
    let group: Option<String> = row.try_get("", "grp")?;
    let group = group.ok_or_else(|| {
        DbErr::Custom("no plugin group role: the app role lacked CREATEROLE at migration".into())
    })?;
    let names = PluginDbNames::new(&database, &group, plugin_id);

    // 64 hex characters: nothing to escape in SQL or in a connection string.
    let candidate = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "INSERT INTO {SECRET_TABLE} (role, password) VALUES ($1, $2) \
             ON CONFLICT (role) DO NOTHING"
        ),
        [names.role.clone().into(), candidate.into()],
    ))
    .await?;
    let password: String = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT password FROM {SECRET_TABLE} WHERE role = $1 FOR UPDATE"),
            [names.role.clone().into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("no login secret for {}", names.role)))?
        .try_get("", "password")?;

    for statement in provision_ddl(&names, &password, &path, read) {
        txn.execute_unprepared(&statement).await?;
    }
    txn.commit().await?;
    Ok(PluginLogin { names, password })
}

/// Retire the role of `plugin_id`, a plugin removed from the fleet: it can no
/// longer log in, and its open sessions end. Its schema and data stay: the
/// plugin id is the plugin's identity, so a later install of the same id is
/// the same plugin, and provisioning lets its role log in again.
pub async fn retire(host: &DatabaseConnection, plugin_id: &str) -> Result<(), DbErr> {
    let row = host
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT current_database()::text AS database, \
                        (SELECT name FROM {GROUP_TABLE}) AS grp"
            ),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("current_database() returned no row".into()))?;
    let database: String = row.try_get("", "database")?;
    let Some(group) = row.try_get::<Option<String>>("", "grp")? else {
        return Ok(());
    };
    let names = PluginDbNames::new(&database, &group, plugin_id);
    host.execute_unprepared(&format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {name}) THEN \
             ALTER ROLE {role} NOLOGIN; \
             PERFORM pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = {name}; \
           END IF; \
         END $$",
        name = quote_literal(&names.role),
        role = quote_ident(&names.role),
    ))
    .await
    .map(drop)
}

/// The statements that bring a plugin's role and schema to their provisioned
/// state, given its stored `password`, the app's `search_path` and the
/// content columns its manifest declares: exactly those are granted, and any
/// other content column a previous version declared is revoked.
fn provision_ddl(
    names: &PluginDbNames,
    password: &str,
    path: &str,
    read: &[SqlReadScope],
) -> Vec<String> {
    let role = quote_ident(&names.role);
    let schema = quote_ident(&names.schema);
    let search_path = if path.trim().is_empty() {
        schema.clone()
    } else {
        format!("{schema}, {path}")
    };
    let mut statements = vec![
        format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {name}) THEN \
                 CREATE ROLE {role} NOLOGIN; \
               END IF; \
             END $$",
            name = quote_literal(&names.role),
        ),
        // CREATE ROLE leaves every other attribute off, and only a role that
        // holds an attribute may name it, even to turn it off, so the app
        // role names just what it holds.
        format!(
            "ALTER ROLE {role} WITH LOGIN INHERIT NOCREATEROLE PASSWORD {}",
            quote_literal(password)
        ),
        format!("GRANT {} TO {role}", quote_ident(&names.group)),
        // The app role joins the plugin role so it may hand it the schema.
        format!("GRANT {role} TO CURRENT_USER"),
        format!("CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION {role}"),
        format!("ALTER ROLE {role} SET search_path TO {search_path}"),
    ];
    let mut tables: Vec<&str> = SQL_READ_SCOPES
        .iter()
        .filter_map(|scope| scope.split_once('.').map(|(t, _)| t))
        .collect();
    tables.dedup();
    for table in tables {
        let columns = |cols: Vec<&str>| {
            cols.iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        };
        statements.push(format!(
            "REVOKE SELECT ({}) ON public.{} FROM {role}",
            columns(content_columns(table)),
            quote_ident(table)
        ));
        let declared: Vec<&str> = read
            .iter()
            .filter(|scope| scope.table() == table)
            .map(SqlReadScope::column)
            .collect();
        if !declared.is_empty() {
            statements.push(format!(
                "GRANT SELECT ({}) ON public.{} TO {role}",
                columns(declared),
                quote_ident(table)
            ));
        }
    }
    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_name_their_schema_and_role_directly() {
        let group = group_role("broccoli");
        let names = PluginDbNames::new("broccoli", &group, "codelink-bracket");
        assert_eq!(names.schema, "plugin_codelink-bracket");
        assert_eq!(names.role, format!("{group}_codelink-bracket"));
        assert!(
            group.starts_with("broccoli_") && group.ends_with("_plugin"),
            "{group}"
        );
    }

    #[test]
    fn long_ids_fit_the_identifier_limit_and_stay_distinct() {
        let a = format!("{}a", "x".repeat(127));
        let b = format!("{}b", "x".repeat(127));
        let (na, nb) = (
            PluginDbNames::new("broccoli", "g", &a),
            PluginDbNames::new("broccoli", "g", &b),
        );
        for name in [&na.schema, &na.role, &nb.schema, &nb.role] {
            assert!(name.len() <= MAX_IDENT_BYTES, "{name}");
        }
        assert_ne!(na.schema, nb.schema);
        assert_ne!(na.role, nb.role);
    }

    #[test]
    fn each_database_gets_its_own_role_but_the_same_schema_name() {
        let (one, two) = (
            PluginDbNames::new("contest_a", "g", "print"),
            PluginDbNames::new("contest_b", "g", "print"),
        );
        assert_ne!(one.role, two.role);
        assert_ne!(group_role("contest_a"), group_role("contest_b"));
        assert_eq!(one.schema, two.schema);
    }

    #[test]
    fn the_search_path_puts_the_plugin_schema_first() {
        let names = PluginDbNames::new("db", "g", "print");
        let search_path = |ddl: Vec<String>| {
            ddl.into_iter()
                .find(|s| s.contains("SET search_path"))
                .expect("a search_path statement")
        };
        let ddl = provision_ddl(&names, "pw", "\"$user\", public", &[]);
        assert_eq!(
            &search_path(ddl),
            &format!(
                "ALTER ROLE {} SET search_path TO \"plugin_print\", \"$user\", public",
                quote_ident(&names.role)
            )
        );
        let ddl = provision_ddl(&names, "pw", "", &[]);
        assert!(search_path(ddl).ends_with("TO \"plugin_print\""));
    }

    #[test]
    fn exactly_the_declared_content_columns_are_granted() {
        let names = PluginDbNames::new("db", "g", "print");
        let read = [SqlReadScope::parse("submission.files").unwrap()];
        let ddl = provision_ddl(&names, "pw", "public", &read);
        let grants: Vec<&String> = ddl
            .iter()
            .filter(|s| s.starts_with("GRANT SELECT"))
            .collect();
        assert_eq!(grants.len(), 1, "{grants:?}");
        assert!(grants[0].starts_with("GRANT SELECT (\"files\") ON public.\"submission\""));
        // Every content table is revoked first, so a dropped scope goes away.
        let revokes = ddl
            .iter()
            .filter(|s| s.starts_with("REVOKE SELECT"))
            .count();
        let mut tables: Vec<&str> = SQL_READ_SCOPES
            .iter()
            .map(|s| s.split_once('.').unwrap().0)
            .collect();
        tables.dedup();
        assert_eq!(revokes, tables.len());
    }

    #[test]
    fn every_content_table_is_plugin_readable() {
        for scope in SQL_READ_SCOPES {
            let table = scope.split_once('.').expect("table.column").0;
            assert!(
                crate::migration::PLUGIN_READABLE.contains(&table),
                "{scope} names a table plugins cannot read"
            );
        }
    }
}
