use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

use crate::migration::Migrator;

pub async fn init_db(db_url: &str) -> Result<DatabaseConnection, DbErr> {
    init_db_with_max_connections(db_url, 100).await
}

/// The restricted, NOLOGIN Postgres role raw plugin SQL runs under. Applied per
/// connection via `SET ROLE` on the restricted pool ([`init_plugin_db`]'s
/// `after_connect`) and re-applied by the raw-path fresh-connection fallback in
/// [`crate::host_funcs::sql`]. Single source of truth for those two `SET ROLE`
/// sites so they cannot drift; the migration DDL that CREATEs/GRANTs the role
/// keeps its own literal since that is statement text, not a role handle.
pub const PLUGIN_DB_ROLE: &str = "broccoli_plugin";

/// The dedicated, least-privilege LOGIN role the RESTRICTED plugin pool
/// authenticates as (see [`provision_restricted_plugin_login`]).
///
/// Why a separate login role at all: the app login role is privileged, and the
/// restricted pool only *downshifts* to [`PLUGIN_DB_ROLE`] via a session-level
/// `SET ROLE`. A plugin that runs `RESET ROLE` (or `SET ROLE <app-login>`)
/// climbs straight back to the privileged app role - and on a POOLED connection
/// that reset persists into the next plugin call. The SQL text guard blocks the
/// statements that do this, but a denylist is inherently leaky. Authenticating
/// the pool as this role instead closes the hole at the engine: a session whose
/// login role is `broccoli_plugin_login` can only ever `RESET ROLE` back to
/// *itself* - a NOINHERIT member of `broccoli_plugin` with no privileges of its
/// own - so an escape lands on a non-privileged role, not the app role.
pub const PLUGIN_DB_LOGIN_ROLE: &str = "broccoli_plugin_login";

/// Username + password to point the restricted plugin pool at a dedicated login
/// role instead of the app role. Applied to the pool's `PgConnectOptions` in
/// [`init_plugin_db`]; the password is never logged.
#[derive(Clone)]
pub struct PluginLoginOverride {
    pub username: String,
    pub password: String,
}

/// Ensure the [`PLUGIN_DB_LOGIN_ROLE`] exists and set its password, using a
/// PRIVILEGED connection (the app role). Returns the login override to point the
/// restricted plugin pool at that role, or `None` if the app role lacks the
/// privilege to manage it - in which case the caller keeps app-role auth for the
/// restricted pool (still defended by the SQL text guard), so this is a
/// hardening, never a hard boot dependency.
///
/// The password is a per-DATABASE persistent random secret: seeded once into a
/// dedicated `plugin_login_secret` table (first writer wins) and read back by
/// every replica on every boot. This is what makes multi-replica deployments
/// safe: all replicas converge on the SAME stored password, so their idempotent
/// `ALTER ROLE ... PASSWORD` set the same value and no replica's pool is ever
/// locked out - not when a peer boots, not when a pooled connection is later
/// replaced. It is deliberately NOT derived from `auth.jwt_secret`: rotating the
/// JWT secret during a rolling deploy would otherwise have the first
/// new-secret replica clobber the password older still-serving replicas are
/// authenticated with. The secret table is REVOKED from `broccoli_plugin`, so a
/// plugin cannot read the login password. Leaking it anyway only unlocks the
/// already-restricted `broccoli_plugin_login` role (which the pool already
/// runs as).
///
/// Why this DDL lives here at runtime and NOT in a versioned migration (the
/// usual home for schema DDL): provisioning must be BEST-EFFORT. A migration
/// runs under `Migrator::up` with `?`, so a role/table creation the app role is
/// not privileged to run would abort boot - turning this hardening into a
/// breaking change for every locked-down deployment. Doing it here lets a
/// failure degrade to app-role auth (return `None`) instead. Do not move it into
/// a migration.
pub async fn provision_restricted_plugin_login(
    privileged: &DatabaseConnection,
) -> Option<PluginLoginOverride> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    // A fresh candidate secret; only the FIRST writer's value persists (ON
    // CONFLICT DO NOTHING), so every replica ends up reading the same stored
    // value. 64 hex chars => no character needing SQL-string or URL escaping.
    let candidate = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    // 1) Persistent secret store, unreadable by the plugin role. `ALTER DEFAULT
    //    PRIVILEGES` auto-grants SELECT on app-role-created tables to
    //    broccoli_plugin, so the REVOKE is load-bearing, not decorative.
    // 2) Seed-if-absent, then read the winning value.
    let setup = [
        // `CREATE TABLE IF NOT EXISTS` is NOT race-safe: two replicas booting at
        // once can both pass the catalog existence check and the loser then hits a
        // duplicate error, which would abort provisioning and silently downgrade
        // that replica to app-role auth for its whole lifetime. Guard it with the
        // same exception pattern as the CREATE ROLE below (the pg_class/pg_type
        // race can surface as duplicate_table, duplicate_object, or unique_violation).
        "DO $$ BEGIN \
           CREATE TABLE plugin_login_secret (\
             singleton boolean PRIMARY KEY DEFAULT true, password text NOT NULL); \
         EXCEPTION WHEN duplicate_table OR duplicate_object OR unique_violation THEN NULL; END $$;"
            .to_string(),
        format!("REVOKE ALL ON plugin_login_secret FROM {PLUGIN_DB_ROLE}"),
        format!(
            "INSERT INTO plugin_login_secret (singleton, password) VALUES (true, '{candidate}') \
             ON CONFLICT (singleton) DO NOTHING"
        ),
    ];
    for stmt in &setup {
        if let Err(e) = privileged.execute_unprepared(stmt).await {
            warn_provision_failed(&e);
            return None;
        }
    }
    let password: String = match privileged
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT password FROM plugin_login_secret WHERE singleton".to_owned(),
        ))
        .await
    {
        Ok(Some(row)) => match row.try_get_by_index(0) {
            Ok(p) => p,
            Err(e) => {
                warn_provision_failed(&e);
                return None;
            }
        },
        Ok(None) => return None,
        Err(e) => {
            warn_provision_failed(&e);
            return None;
        }
    };

    // Create-if-missing as NOLOGIN (so a half-provisioned role never sits around
    // LOGIN-able without the intended password), then flip to LOGIN with the
    // stored password. NOINHERIT means a `RESET ROLE` onto this role yields NO
    // privileges until an explicit `SET ROLE broccoli_plugin`; membership in
    // broccoli_plugin is what the after-connect `SET ROLE` relies on. The
    // NO{SUPERUSER,CREATEDB,CREATEROLE,REPLICATION,BYPASSRLS} flags keep it
    // strictly at or below broccoli_plugin.
    let create = format!(
        "DO $$ BEGIN \
           CREATE ROLE {role} NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS IN ROLE {parent}; \
         EXCEPTION WHEN duplicate_object THEN NULL; END $$;",
        role = PLUGIN_DB_LOGIN_ROLE,
        parent = PLUGIN_DB_ROLE,
    );
    // Re-assert membership + flags even if the role pre-existed from an older
    // boot, then set LOGIN and the stored password. The password is our own hex,
    // so string interpolation carries no injection; sqlx statement logging is
    // off on this (the app) pool. It CAN still reach the Postgres server log if
    // the operator sets `log_statement = 'ddl'|'all'`, but the value only unlocks
    // the already-restricted `broccoli_plugin_login` role, so that exposure is
    // low-value; operators who log DDL and object can instead point
    // `database.plugin_url` at a DBA-managed role.
    let grant = format!(
        "GRANT {parent} TO {role}",
        parent = PLUGIN_DB_ROLE,
        role = PLUGIN_DB_LOGIN_ROLE,
    );
    let alter = format!(
        "ALTER ROLE {role} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '{password}'",
        role = PLUGIN_DB_LOGIN_ROLE,
    );

    for stmt in [create, grant, alter] {
        if let Err(e) = privileged.execute_unprepared(&stmt).await {
            warn_provision_failed(&e);
            return None;
        }
    }

    tracing::info!(
        role = PLUGIN_DB_LOGIN_ROLE,
        "Provisioned dedicated least-privilege login for the restricted plugin SQL pool"
    );
    Some(PluginLoginOverride {
        username: PLUGIN_DB_LOGIN_ROLE.to_string(),
        password,
    })
}

fn warn_provision_failed(e: &sea_orm::DbErr) {
    tracing::warn!(
        error = %e,
        "Could not provision the dedicated restricted plugin login role; \
         falling back to app-role auth for the plugin SQL pool (the SQL text \
         guard remains the active defense). Grant the app role CREATEROLE, or \
         set database.plugin_url to a DBA-managed role, to enable engine-level \
         role isolation."
    );
}

/// How the RESTRICTED plugin SQL pool ended up authenticating -- which decides
/// whether raw plugin SQL (`host.db.*`) is safe to run at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestrictedPluginAuth {
    /// Auto-provisioned dedicated NOINHERIT login `broccoli_plugin_login`: a role
    /// escape lands on a powerless role at the ENGINE, independent of the SQL text
    /// guard. Raw plugin SQL is safe to enable.
    DedicatedLogin,
    /// Operator-configured `database.plugin_url`: a DBA-managed role the operator
    /// vouches for. Raw plugin SQL is enabled; the isolation is the operator's call.
    OperatorUrl,
    /// Neither was available, so the pool degraded to the privileged APP role with
    /// only the best-effort SQL text guard defending it. The caller MUST fail
    /// closed -- disable raw plugin SQL rather than execute it as the privileged
    /// role (see `host_funcs::sql::disable_raw_plugin_sql`).
    AppRoleDegraded,
}

/// Single source of truth for [`RestrictedPluginAuth`]: an operator-configured URL
/// is trusted as-is; otherwise a dedicated login means engine-level isolation and
/// its absence means we fell back to the app role. Pure, so the classification is
/// unit-testable without a live database.
pub fn classify_restricted_auth(
    operator_url_configured: bool,
    has_dedicated_login: bool,
) -> RestrictedPluginAuth {
    if operator_url_configured {
        RestrictedPluginAuth::OperatorUrl
    } else if has_dedicated_login {
        RestrictedPluginAuth::DedicatedLogin
    } else {
        RestrictedPluginAuth::AppRoleDegraded
    }
}

/// How the restricted plugin pool should authenticate, and the fresh-connection
/// fallback that must match it: the pool itself, the URL it connected on, and the
/// login override it used (`None` = app-role auth).
pub struct RestrictedPluginPool {
    pub pool: DatabaseConnection,
    pub fallback_url: String,
    pub fallback_login: Option<PluginLoginOverride>,
    /// How the pool authenticated. `AppRoleDegraded` means the caller must disable
    /// raw plugin SQL (fail closed). See [`RestrictedPluginAuth`].
    pub auth: RestrictedPluginAuth,
}

/// Resolve and open the RESTRICTED plugin pool: point it at a DBA-managed
/// `plugin_url` when configured, else auto-provision `broccoli_plugin_login` and
/// override the app URL's credentials. Connecting can fail even when the role
/// exists (e.g. a pg_hba username allow-list), so an auto-provisioned login that
/// cannot connect degrades to app-role auth rather than aborting boot; an
/// explicitly configured `plugin_url` that fails is a real misconfiguration and
/// propagates. Extracted from `ServerRuntime::build` so the degrade-not-abort
/// branch is unit-testable.
pub async fn resolve_restricted_plugin_pool(
    privileged: &DatabaseConnection,
    app_url: &str,
    plugin_url: Option<&str>,
    max_connections: u32,
) -> Result<RestrictedPluginPool, DbErr> {
    let (primary_url, login) = match plugin_url {
        Some(url) => (url.to_string(), None),
        None => (
            app_url.to_string(),
            provision_restricted_plugin_login(privileged).await,
        ),
    };
    let mut resolved =
        connect_restricted_pool_with_fallback(app_url, primary_url, login, max_connections).await?;
    // An operator-configured `plugin_url` is trusted as-is; otherwise the presence
    // of a dedicated login decides engine isolation vs. an app-role degrade.
    resolved.auth =
        classify_restricted_auth(plugin_url.is_some(), resolved.fallback_login.is_some());
    Ok(resolved)
}

/// Connect the restricted pool on `primary_url`/`primary_login`, falling back to
/// app-role auth ONLY when a dedicated `primary_login` was attempted and refused.
/// A `None` login (ops-configured `plugin_url`, or provisioning already
/// declined) is never downgraded further - its failure propagates.
async fn connect_restricted_pool_with_fallback(
    app_url: &str,
    primary_url: String,
    primary_login: Option<PluginLoginOverride>,
    max_connections: u32,
) -> Result<RestrictedPluginPool, DbErr> {
    match init_plugin_db(&primary_url, max_connections, primary_login.clone()).await {
        Ok(pool) => Ok(RestrictedPluginPool {
            pool,
            fallback_url: primary_url,
            // Provisional (operator URLs are upgraded to `OperatorUrl` by the
            // caller, which alone knows whether a `plugin_url` was configured).
            auth: classify_restricted_auth(false, primary_login.is_some()),
            fallback_login: primary_login,
        }),
        Err(e) if primary_login.is_some() => {
            tracing::warn!(
                error = %e,
                "Restricted plugin pool could not connect as the dedicated login role; \
                 falling back to app-role auth (raw plugin SQL will be disabled)"
            );
            let pool = init_plugin_db(app_url, max_connections, None).await?;
            Ok(RestrictedPluginPool {
                pool,
                fallback_url: app_url.to_string(),
                fallback_login: None,
                auth: RestrictedPluginAuth::AppRoleDegraded,
            })
        }
        Err(e) => Err(e),
    }
}

/// The RESTRICTED plugin pool for the raw `sql` capability (`host.db.*`).
///
/// Two properties, both essential:
///
/// 1. **Isolation** - like every plugin pool, it is SEPARATE from the main
///    server pool used by HTTP handlers, the dispatcher, and the
///    windowed-evaluate driver. Under load the shared pool produces spurious
///    server-side "invalid byte sequence ... 0x00" errors on byte-clean plugin
///    INSERTs - connection-protocol desync inherited from OTHER operations on
///    the same pool (see docs/plans/2026-06-25-nul-byte-root-cause.md). Plugin
///    host functions always run their DB call to completion (sync `block_on` on
///    a blocking thread, never cancelled), so a pool used ONLY by them stays
///    clean. No schema-sync/seeding here - the main pool already ran it.
///
/// 2. **Bounded privilege** - every connection runs `SET ROLE broccoli_plugin`
///    before the plugin can use it, via sqlx's per-connection `after_connect`
///    hook. `broccoli_plugin` is a NOLOGIN role (created + granted in
///    [`init_db_with_max_connections`]) that the app role is a MEMBER of. By
///    product decision (migrations m0003/m0004) it has full read + write DML
///    (SELECT/INSERT/UPDATE/DELETE) on every table, so raw plugin SQL can freely
///    read and mutate contest DATA. What it still CANNOT do, and what this role
///    boundary exists to enforce, is: schema DDL on core tables (DROP/ALTER are
///    owner-only, and the role does not OWN core tables), TRUNCATE, reaching the
///    app role's DDL/superuser powers, or reading/writing `plugin_login_secret`.
///    A plugin's OWN tables - created via raw `CREATE TABLE` under this role -
///    are owned by `broccoli_plugin`, so it keeps full DDL on those. The gated
///    `host.submission.*` / `host.storage.*` / `config:write` host fns still run
///    on the PRIVILEGED pool ([`init_plugin_db_privileged`], which does NOT
///    `SET ROLE`) - server-owned structured writes, independent of what raw SQL
///    may now also do.
///
/// `login`, when `Some`, overrides the URL's username/password so the pool
/// authenticates as the dedicated least-privilege [`PLUGIN_DB_LOGIN_ROLE`]
/// rather than the (privileged) app login role. This is what makes a plugin's
/// `RESET ROLE` land on a non-privileged role instead of the app role; the
/// after-connect `SET ROLE broccoli_plugin` still runs on top for normal
/// operation (and so a `CREATE TABLE` is owned by the shared plugin role, not
/// the login role). When `None` the pool keeps app-role auth, defended by the
/// SQL text guard alone.
pub async fn init_plugin_db(
    db_url: &str,
    max_connections: u32,
    login: Option<PluginLoginOverride>,
) -> Result<DatabaseConnection, DbErr> {
    // `sqlx::ConnectOptions` (trait) brings `disable_statement_logging` into
    // scope; it is distinct from `sea_orm::ConnectOptions` (struct) used below.
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let pool_options = PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(max_connections.min(2))
        // sea_orm's ConnectOptions collapses connect_timeout + acquire_timeout
        // onto sqlx's single acquire_timeout; 30s matches the privileged pool.
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(true)
        // The unbypassable choke point: every physical connection downshifts to
        // the bounded `broccoli_plugin` role at connect time. `SET ROLE` is
        // session-level and persists for the connection's lifetime, so pooled
        // reuse stays on the plugin role (no schema DDL on core, no app-role
        // powers) even though it may now read/write data freely.
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sea_orm::sqlx::Executor::execute(
                    conn,
                    format!("SET ROLE {PLUGIN_DB_ROLE}").as_str(),
                )
                .await
                .map(|_| ())
            })
        });

    let mut connect_options = db_url
        .parse::<PgConnectOptions>()
        .map_err(|e| DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e.into())))?
        .disable_statement_logging();
    if let Some(login) = &login {
        // Overriding on PgConnectOptions (not by rewriting the URL string) means
        // no percent-encoding concerns for the credentials.
        connect_options = connect_options
            .username(&login.username)
            .password(&login.password);
    }

    let pool = pool_options
        .connect_with(connect_options)
        .await
        .map_err(|e| DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e.into())))?;

    Ok(sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(
        pool,
    ))
}

/// The PRIVILEGED plugin pool: identical to [`init_plugin_db`] (same isolation,
/// same pool sizing/timeouts) but WITHOUT the `SET ROLE broccoli_plugin`
/// downshift, so its connections run as the full app role.
///
/// The gated, server-owned structured host functions run here, NOT on the
/// `broccoli_plugin` pool: `host.submission.*`, `host.storage.*`, and
/// `config:write`. Each builds structured, plugin-scoped SQL owned by the server,
/// and runs as the app role so it is not bound by the plugin role's boundaries
/// (no schema DDL on core, no `plugin_login_secret`). That the raw `sql`
/// capability may now also read/write data on its own pool does not change this:
/// the structured path stays server-controlled and app-role. Both pools point at
/// the same database URL; only the effective role differs.
pub async fn init_plugin_db_privileged(
    db_url: &str,
    max_connections: u32,
) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(db_url.to_owned());
    opt.max_connections(max_connections)
        .min_connections(max_connections.min(2))
        .connect_timeout(Duration::from_secs(30))
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(true)
        .sqlx_logging(false);
    Database::connect(opt).await
}

pub async fn init_db_with_max_connections(
    db_url: &str,
    max_connections: u32,
) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(db_url.to_owned());

    opt.max_connections(max_connections)
        .min_connections(max_connections.min(5))
        .connect_timeout(Duration::from_secs(30))
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        // A plugin DB host call can be cancelled mid-flight (e.g. a detached
        // evaluate timeout aborts the Extism call while it is inside
        // `block_on(execute_raw)`), which can return a pooled connection with
        // unread protocol bytes. Reusing such a connection surfaces stale
        // framing bytes (message length headers contain 0x00) as a spurious
        // "invalid byte sequence for encoding UTF8: 0x00" on the NEXT, clean
        // statement. Liveness-check connections on acquire so sqlx discards a
        // desynced connection instead of handing it to the next query.
        .test_before_acquire(true)
        .sqlx_logging(false);

    let db = Database::connect(opt).await?;

    crate::migration::prepare_user_display_name(&db).await?;

    // Create/update the core tables from the entity definitions.
    db.get_schema_registry("server::entity::*")
        .sync(&db)
        .await?;

    // Apply the versioned, tracked supplementary DDL (column evolutions, the
    // one-time data backfill, and the `broccoli_plugin` least-privilege role +
    // read-narrowing grants/view) that the entity `sync()` above does not cover.
    // Recorded in `seaql_migrations` so each runs once and its failures surface
    // instead of being swallowed; idempotent, so an existing deployment records
    // them as no-ops on first run. See `crate::migration`.
    Migrator::up(&db, None).await?;

    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ConnectionTrait;

    /// Pure classification of the restricted pool's end-state — the single input
    /// to the runtime's raw-SQL fail-closed decision. Operator `plugin_url` wins
    /// over an auto-provisioned login (a DBA-managed role is authoritative); a
    /// dedicated login is the auto-provisioned secure path; only the no-url,
    /// no-login case degrades to the privileged app role, where the caller MUST
    /// disable raw plugin SQL. No container needed.
    #[test]
    fn classify_restricted_auth_covers_all_end_states() {
        // operator_url wins even if a login also exists (url is authoritative).
        assert_eq!(
            classify_restricted_auth(true, true),
            RestrictedPluginAuth::OperatorUrl
        );
        assert_eq!(
            classify_restricted_auth(true, false),
            RestrictedPluginAuth::OperatorUrl
        );
        // no url, dedicated login provisioned -> secure auto path.
        assert_eq!(
            classify_restricted_auth(false, true),
            RestrictedPluginAuth::DedicatedLogin
        );
        // no url, no login -> the degrade path the caller must fail closed on.
        assert_eq!(
            classify_restricted_auth(false, false),
            RestrictedPluginAuth::AppRoleDegraded
        );
    }

    /// The plugin role may now READ and WRITE (DML) any core table by product
    /// decision (m0003 reads, m0004 writes), but core schema DDL (`DROP`/`ALTER`)
    /// stays owner-only, so a plugin cannot corrupt/drop the schema. It retains
    /// full DDL on its OWN tables. Drives the REAL migration + REAL pools against
    /// a live Postgres.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broccoli_plugin_can_read_write_core_but_not_ddl_it_and_owns_its_tables() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        // Full app migration: syncs the core schema AND applies the plugin role's
        // read (m0003) + write (m0004) grants.
        let app = init_db_with_max_connections(&url, 5)
            .await
            .expect("app migration + schema sync");

        // RESTRICTED pool: every connection `SET ROLE broccoli_plugin`.
        let restricted = init_plugin_db(&url, 2, None)
            .await
            .expect("restricted plugin pool");
        // PRIVILEGED pool: runs as the app role (no SET ROLE).
        let privileged = init_plugin_db_privileged(&url, 2)
            .await
            .expect("privileged plugin pool");

        // POSITIVE: the plugin role can READ core state.
        restricted
            .execute_unprepared("SELECT id FROM submission LIMIT 1")
            .await
            .expect("plugin role must be able to SELECT from a core table");

        // POSITIVE: core WRITES (UPDATE/DELETE) are now permitted.
        restricted
            .execute_unprepared("UPDATE submission SET score = 999")
            .await
            .expect("plugin role must be able to UPDATE a core table");
        restricted
            .execute_unprepared("DELETE FROM test_case_result")
            .await
            .expect("plugin role must be able to DELETE from a core table");

        // POSITIVE: INSERT into a serial-PK table works - proving BOTH the
        // future-table default-privilege grant AND the sequence USAGE grant
        // landed (drawing an id via nextval needs sequence USAGE). The app role
        // creates the table AFTER migration, so only the `ALTER DEFAULT
        // PRIVILEGES` grants (not the one-time `ALL TABLES`/`ALL SEQUENCES`) can
        // make this succeed.
        app.execute_unprepared("CREATE TABLE seqtest (id serial PRIMARY KEY, v int)")
            .await
            .expect("app role creates a serial table post-migration");
        restricted
            .execute_unprepared("INSERT INTO seqtest (v) VALUES (1)")
            .await
            .expect("plugin role must INSERT into a future table and draw a serial id");

        // NEGATIVE: schema DDL (DROP) is still denied - non-owner.
        let drop_err = restricted
            .execute_unprepared("DROP TABLE submission")
            .await
            .expect_err("DROP TABLE submission must stay denied for broccoli_plugin");
        assert!(
            drop_err
                .to_string()
                .to_lowercase()
                .contains("must be owner"),
            "expected must-be-owner DROP, got: {drop_err}"
        );

        // NEGATIVE: schema DDL (ALTER) is still denied - non-owner.
        let alter_err = restricted
            .execute_unprepared("ALTER TABLE submission ADD COLUMN pwn text")
            .await
            .expect_err("ALTER TABLE submission must stay denied for broccoli_plugin");
        assert!(
            alter_err
                .to_string()
                .to_lowercase()
                .contains("must be owner"),
            "expected must-be-owner ALTER, got: {alter_err}"
        );

        // POSITIVE: the plugin's OWN table (created under this role) is fully
        // read/write/DDL-able - `broccoli_plugin` owns it.
        restricted
            .execute_unprepared("CREATE TABLE plugin_owned_kv (k text primary key, v text)")
            .await
            .expect("restricted role must be able to CREATE its own table");
        restricted
            .execute_unprepared("INSERT INTO plugin_owned_kv (k, v) VALUES ('a', '1')")
            .await
            .expect("restricted role must be able to write its own table");
        restricted
            .execute_unprepared("UPDATE plugin_owned_kv SET v = '2' WHERE k = 'a'")
            .await
            .expect("restricted role must be able to update its own table");
        restricted
            .execute_unprepared("DROP TABLE plugin_owned_kv")
            .await
            .expect("restricted role must be able to drop its own table");

        // POSITIVE: the PRIVILEGED pool CAN write core - this is the pool that
        // backs host.submission.* / host.storage.* / config:write, so judging's
        // structured writes keep working.
        privileged
            .execute_unprepared("UPDATE submission SET score = 0")
            .await
            .expect("privileged pool must be able to write core tables");
    }

    /// SECURITY - engine-level role-escalation containment (the root-cause fix
    /// behind the SQL text guard).
    ///
    /// When the restricted pool authenticates as the dedicated
    /// [`PLUGIN_DB_LOGIN_ROLE`], a plugin that escapes the after-connect
    /// `SET ROLE broccoli_plugin` via `RESET ROLE` / `SET ROLE <app>` must land
    /// on a NON-privileged role, never the app role. This drives the REAL
    /// provisioning + REAL restricted pool (with the login override) against a
    /// live Postgres and asserts the escape is powerless. `max_connections = 1`
    /// forces every statement onto the one physical connection, so a persisted
    /// `RESET ROLE` carries into the next statement exactly as it would across
    /// pooled plugin calls.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dedicated_login_contains_role_escalation_escape() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let app = init_db_with_max_connections(&url, 5)
            .await
            .expect("app migration + schema sync");

        // Provision the dedicated login using the privileged app connection.
        let login = provision_restricted_plugin_login(&app)
            .await
            .expect("app role (superuser here) can provision the dedicated login");
        assert_eq!(login.username, PLUGIN_DB_LOGIN_ROLE);

        // Single-connection restricted pool authenticating as the dedicated role.
        let restricted = init_plugin_db(&url, 1, Some(login))
            .await
            .expect("restricted plugin pool on the dedicated login");

        // Baseline: normal operation runs as broccoli_plugin (after-connect SET
        // ROLE) and can read core.
        restricted
            .execute_unprepared("SELECT id FROM submission LIMIT 1")
            .await
            .expect("dedicated-login pool must read core as broccoli_plugin");

        // ESCAPE 1 - RESET ROLE drops to the login role, which is NOT the app
        // role: it is broccoli_plugin_login, a NOINHERIT member of broccoli_plugin
        // with no privileges of its own.
        restricted
            .execute_unprepared("RESET ROLE")
            .await
            .expect("RESET ROLE itself is allowed; it just lands on the login role");
        let who = restricted
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT current_user, session_user".to_owned(),
            ))
            .await
            .expect("query current/session user")
            .expect("one row");
        let current_user: String = who.try_get_by_index(0).expect("current_user");
        let session_user: String = who.try_get_by_index(1).expect("session_user");
        assert_eq!(
            current_user, PLUGIN_DB_LOGIN_ROLE,
            "RESET ROLE must land on the dedicated login, not the app role"
        );
        assert_eq!(
            session_user, PLUGIN_DB_LOGIN_ROLE,
            "the session must be authenticated as the dedicated login, not the app role"
        );
        assert_ne!(
            session_user, "postgres",
            "the restricted pool must NOT be authenticated as the privileged app role"
        );

        // After the escape, a core WRITE is still denied (login role has no privs
        // of its own; it is NOINHERIT so it does not even carry broccoli_plugin's
        // SELECT until an explicit SET ROLE).
        let write_err = restricted
            .execute_unprepared("UPDATE submission SET score = 999")
            .await
            .expect_err("core write after RESET ROLE must stay denied");
        assert!(
            write_err
                .to_string()
                .to_lowercase()
                .contains("permission denied"),
            "expected permission-denied write after escape, got: {write_err}"
        );

        // ESCAPE 2 - directly targeting the app role fails: the login role is not
        // a member of it, so Postgres refuses the SET ROLE.
        let set_app_err = restricted
            .execute_unprepared("SET ROLE postgres")
            .await
            .expect_err("SET ROLE to the app role must be refused for the dedicated login");
        let msg = set_app_err.to_string().to_lowercase();
        assert!(
            msg.contains("permission denied") || msg.contains("cannot") || msg.contains("must be"),
            "expected a refusal escalating to the app role, got: {set_app_err}"
        );

        // The login password lives in `plugin_login_secret`; a plugin (running as
        // broccoli_plugin) must NOT be able to read it, or it could learn its own
        // pool credentials. The REVOKE in provisioning is what enforces this.
        restricted
            .execute_unprepared("SET ROLE broccoli_plugin")
            .await
            .expect("re-enter the broccoli_plugin role");
        let secret_err = restricted
            .execute_unprepared("SELECT password FROM plugin_login_secret")
            .await
            .expect_err("plugin role must not read the login secret table");
        assert!(
            secret_err
                .to_string()
                .to_lowercase()
                .contains("permission denied"),
            "expected permission-denied reading plugin_login_secret, got: {secret_err}"
        );
    }

    /// Availability: when a dedicated login was attempted but the connection is
    /// refused (here simulated with a non-existent login role, standing in for a
    /// pg_hba username allow-list), the restricted pool must DEGRADE to app-role
    /// auth and still come up - never abort boot. Drives the real fallback branch
    /// of `connect_restricted_pool_with_fallback`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_pool_degrades_to_app_role_when_dedicated_login_cannot_connect() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let _app = init_db_with_max_connections(&url, 5)
            .await
            .expect("app migration + schema sync");

        // A login that can never authenticate: the role does not exist.
        let unusable = PluginLoginOverride {
            username: "no_such_plugin_login".to_string(),
            password: "nope".to_string(),
        };
        let resolved = connect_restricted_pool_with_fallback(&url, url.clone(), Some(unusable), 2)
            .await
            .expect("a refused dedicated login must degrade, not error");

        // Degraded to app-role auth, and the returned pool actually works.
        assert!(
            resolved.fallback_login.is_none(),
            "must have downgraded to app-role auth"
        );
        assert_eq!(
            resolved.auth,
            RestrictedPluginAuth::AppRoleDegraded,
            "a refused dedicated login must classify as the app-role degrade path so \
             the caller fails raw plugin SQL closed"
        );
        assert_eq!(
            resolved.fallback_url, url,
            "fallback must report the app URL"
        );
        resolved
            .pool
            .execute_unprepared("SELECT 1")
            .await
            .expect("the degraded app-role pool must be usable");
    }

    /// The plugin read DENY-LIST was removed by product decision (m0003): plugins
    /// may `SELECT` every table, including formerly-restricted ones. This drives
    /// the REAL migration + REAL restricted pool against a live Postgres and
    /// asserts the previously-denied tables are now readable. (Credential columns
    /// they expose are argon2-hashed at rest; contest admins are trusted.) This
    /// test covers reads only; `broccoli_plugin_can_read_write_core_but_not_ddl_it_and_owns_its_tables`
    /// covers that the plugin role may now WRITE (DML) core tables while core DDL
    /// stays owner-only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broccoli_plugin_can_read_all_tables_after_denylist_removed() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let _app = init_db_with_max_connections(&url, 5)
            .await
            .expect("app migration + schema sync");
        let restricted = init_plugin_db(&url, 2, None)
            .await
            .expect("restricted plugin pool");

        // Every formerly deny-listed table is now SELECT-able by the plugin role.
        for table in [
            r#""user""#,
            "refresh_tokens",
            "role",
            "role_permission",
            "user_role",
            "plugin",
            "plugin_config",
            "plugin_storage",
            "idempotency_key",
            "dead_letter_message",
        ] {
            restricted
                .execute_unprepared(&format!("SELECT * FROM {table} LIMIT 1"))
                .await
                .unwrap_or_else(|e| {
                    panic!("SELECT from {table} must be allowed after deny-list removal, got: {e}")
                });
        }

        // Contest-data tables and the curated view stay readable too.
        restricted
            .execute_unprepared("SELECT id FROM submission LIMIT 1")
            .await
            .expect("contest-data reads must still work");
        restricted
            .execute_unprepared("SELECT id, username FROM plugin_user_public LIMIT 1")
            .await
            .expect("the curated user view must still be readable");
    }
}
