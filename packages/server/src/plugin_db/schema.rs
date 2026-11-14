//! A plugin's database identity: the login role its SQL runs as, the schema
//! that role owns, and the DDL that provisions both.
//!
//! Every name is derived from the plugin id alone, so the host holds no
//! per-plugin knowledge. The role is a member of [`PLUGIN_GROUP_ROLE`], which
//! carries the read grants on host relations (migration m0001), and owns
//! schema `plugin_<id>`, where it has full DDL and DML. It has no other
//! privilege: it cannot write host state, read secrets, or reach another
//! plugin's schema.

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement, TransactionTrait};
use sha2::{Digest, Sha256};

/// The NOLOGIN group role every plugin role belongs to. It holds `USAGE` on
/// schema `public` and `SELECT` on the plugin-readable host relations, and
/// nothing else.
pub const PLUGIN_GROUP_ROLE: &str = "broccoli_plugin";

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

/// The schema and the login role of one plugin in one database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDbNames {
    /// `plugin_<id>`: the schema the role owns, first on its `search_path`.
    pub schema: String,
    /// The login role. Roles are shared by every database of a cluster, so
    /// the name carries a digest of the database name and two databases on
    /// one cluster never share a plugin role.
    pub role: String,
}

impl PluginDbNames {
    pub fn new(database: &str, plugin_id: &str) -> Self {
        let database_tag = hex::encode(&Sha256::digest(database.as_bytes())[..4]);
        Self {
            schema: bounded_ident("plugin_", plugin_id),
            role: bounded_ident(&format!("broccoli_{database_tag}_"), plugin_id),
        }
    }
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
/// [`SECRET_TABLE`], which no plugin can lock. The first writer's password
/// wins and every replica reads it back, so all replicas connect with the
/// same password.
///
/// The role's `search_path` is its schema followed by the app's own
/// `search_path`, so unqualified DDL lands in the plugin's schema and
/// unqualified reads find the host relations.
pub async fn provision(host: &DatabaseConnection, plugin_id: &str) -> Result<PluginLogin, DbErr> {
    let txn = host.begin().await?;
    let row = txn
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS database, current_setting('search_path') AS path",
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("current_database() returned no row".into()))?;
    let database: String = row.try_get("", "database")?;
    let path: String = row.try_get("", "path")?;
    let names = PluginDbNames::new(&database, plugin_id);

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

    for statement in provision_ddl(&names, &password, &path) {
        txn.execute_unprepared(&statement).await?;
    }
    txn.commit().await?;
    Ok(PluginLogin { names, password })
}

/// The statements that bring a plugin's role and schema to their provisioned
/// state, given its stored `password` and the app's `search_path`.
fn provision_ddl(names: &PluginDbNames, password: &str, path: &str) -> Vec<String> {
    let role = quote_ident(&names.role);
    let schema = quote_ident(&names.schema);
    let search_path = if path.trim().is_empty() {
        schema.clone()
    } else {
        format!("{schema}, {path}")
    };
    vec![
        format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {name}) THEN \
                 CREATE ROLE {role} NOLOGIN; \
               END IF; \
             END $$",
            name = quote_literal(&names.role),
        ),
        format!(
            "ALTER ROLE {role} WITH LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS PASSWORD {}",
            quote_literal(password)
        ),
        format!("GRANT {PLUGIN_GROUP_ROLE} TO {role}"),
        // The app role joins the plugin role so it may hand it the schema.
        format!("GRANT {role} TO CURRENT_USER"),
        format!("CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION {role}"),
        format!("ALTER ROLE {role} SET search_path TO {search_path}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_name_their_schema_and_role_directly() {
        let names = PluginDbNames::new("broccoli", "codelink-bracket");
        assert_eq!(names.schema, "plugin_codelink-bracket");
        assert!(names.role.starts_with("broccoli_"), "{}", names.role);
        assert!(names.role.ends_with("_codelink-bracket"), "{}", names.role);
    }

    #[test]
    fn long_ids_fit_the_identifier_limit_and_stay_distinct() {
        let a = format!("{}a", "x".repeat(127));
        let b = format!("{}b", "x".repeat(127));
        let (na, nb) = (
            PluginDbNames::new("broccoli", &a),
            PluginDbNames::new("broccoli", &b),
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
            PluginDbNames::new("contest_a", "print"),
            PluginDbNames::new("contest_b", "print"),
        );
        assert_ne!(one.role, two.role);
        assert_eq!(one.schema, two.schema);
    }

    #[test]
    fn the_search_path_puts_the_plugin_schema_first() {
        let names = PluginDbNames::new("db", "print");
        let ddl = provision_ddl(&names, "pw", "\"$user\", public");
        assert_eq!(
            ddl.last().unwrap(),
            &format!(
                "ALTER ROLE {} SET search_path TO \"plugin_print\", \"$user\", public",
                quote_ident(&names.role)
            )
        );
        let ddl = provision_ddl(&names, "pw", "");
        assert!(ddl.last().unwrap().ends_with("TO \"plugin_print\""));
    }
}
