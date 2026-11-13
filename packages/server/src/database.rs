use common::db::{PgPoolSpec, PgSession};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

use crate::migration::Migrator;

/// `application_name` of the server's own pools (`pg_stat_activity`).
pub const APP_NAME: &str = "broccoli-server";
/// `application_name` of the plugin host-function pools, so plugin SQL is
/// distinguishable from the server's own in `pg_stat_activity`.
pub const PLUGIN_APP_NAME: &str = "broccoli-server-plugin";

/// The session every pooled server connection runs with.
pub fn app_session(config: &crate::config::DatabaseConfig) -> PgSession {
    PgSession::new(
        APP_NAME,
        config.statement_timeout_secs,
        config.tcp_keepalives,
    )
}

/// The same bounds as [`app_session`], labelled as plugin traffic.
pub fn plugin_session(app: &PgSession) -> PgSession {
    PgSession {
        application_name: PLUGIN_APP_NAME,
        ..app.clone()
    }
}

/// Migrated main pool with default settings (tests and tools).
pub async fn init_db(db_url: &str) -> Result<DatabaseConnection, DbErr> {
    init_db_with_max_connections(
        db_url,
        100,
        &app_session(&crate::config::DatabaseConfig::default()),
    )
    .await
}

/// The restricted, NOLOGIN Postgres role raw plugin SQL runs under. Applied per
/// connection via `SET ROLE` on the restricted pool ([`init_plugin_db`]'s
/// `after_connect`). The migration DDL that CREATEs/GRANTs the role keeps its
/// own literal since that is statement text, not a role handle.
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
    use sea_orm::{DbBackend, Statement};

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

/// The restricted plugin pool and how it authenticated.
pub struct RestrictedPluginPool {
    pub pool: DatabaseConnection,
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
    session: &PgSession,
) -> Result<RestrictedPluginPool, DbErr> {
    let (primary_url, login) = match plugin_url {
        Some(url) => (url.to_string(), None),
        None => (
            app_url.to_string(),
            provision_restricted_plugin_login(privileged).await,
        ),
    };
    let mut resolved = connect_restricted_pool_with_fallback(
        app_url,
        primary_url,
        login,
        max_connections,
        session,
    )
    .await?;
    // An operator-configured `plugin_url` is trusted as-is; otherwise the presence
    // of a dedicated login decides engine isolation vs. an app-role degrade.
    resolved.auth = classify_restricted_auth(
        plugin_url.is_some(),
        resolved.auth == RestrictedPluginAuth::DedicatedLogin,
    );
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
    session: &PgSession,
) -> Result<RestrictedPluginPool, DbErr> {
    match init_plugin_db(
        &primary_url,
        max_connections,
        primary_login.clone(),
        session,
    )
    .await
    {
        Ok(pool) => Ok(RestrictedPluginPool {
            pool,
            // Provisional (operator URLs are upgraded to `OperatorUrl` by the
            // caller, which alone knows whether a `plugin_url` was configured).
            auth: classify_restricted_auth(false, primary_login.is_some()),
        }),
        Err(e) if primary_login.is_some() => {
            tracing::warn!(
                error = %e,
                "Restricted plugin pool could not connect as the dedicated login role; \
                 falling back to app-role auth (raw plugin SQL will be disabled)"
            );
            let pool = init_plugin_db(app_url, max_connections, None, session).await?;
            Ok(RestrictedPluginPool {
                pool,
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
///    windowed-evaluate driver, so plugin SQL can neither exhaust the server's
///    connections nor wait behind them. No schema-sync/seeding here -
///    [`BootSession`] already ran it.
///
/// 2. **Bounded privilege** - every connection runs `SET ROLE broccoli_plugin`
///    before the plugin can use it, via sqlx's per-connection `after_connect`
///    hook. `broccoli_plugin` is a NOLOGIN role (created + granted by the
///    migrations in [`BootSession::migrate`]) that the app role is a MEMBER of. By
///    product decision it has full read + write DML
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
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    let pool_options = PgPoolSpec::new(max_connections, 2)
        .pool_options()
        // The unbypassable choke point: every physical connection downshifts to
        // the bounded `broccoli_plugin` role at connect time. `SET ROLE` is
        // session-level and persists for the connection's lifetime, so pooled
        // reuse stays on the plugin role (no schema DDL on core, no app-role
        // powers) even though it may now read/write data freely.
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sea_orm::sqlx::Executor::execute(
                    conn,
                    sea_orm::sqlx::AssertSqlSafe(format!("SET ROLE {PLUGIN_DB_ROLE}")),
                )
                .await
                .map(|_| ())
            })
        });

    let mut connect_options = common::db::pg_connect_options(db_url, &plugin_session(session))?;
    if let Some(login) = &login {
        // Overriding on PgConnectOptions (not by rewriting the URL string) means
        // no percent-encoding concerns for the credentials.
        connect_options = connect_options
            .username(&login.username)
            .password(&login.password);
    }

    common::db::connect_with(pool_options, connect_options).await
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
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    common::db::connect(
        db_url,
        &PgPoolSpec::new(max_connections, 2),
        &plugin_session(session),
    )
    .await
}

/// Bring the schema up to date (under the boot lock), then open the main pool.
/// For tests and tools; the server binary drives [`BootSession`] itself so
/// seeding runs under the same lock.
pub async fn init_db_with_max_connections(
    db_url: &str,
    max_connections: u32,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    let boot = BootSession::acquire(db_url).await?;
    boot.migrate().await?;
    boot.release().await?;
    connect_main_pool(db_url, max_connections, session).await
}

/// The main pool (handlers, dispatcher, background loops). No schema work.
pub async fn connect_main_pool(
    db_url: &str,
    max_connections: u32,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    // A statement future dropped mid-flight (client disconnect, the request
    // `TimeoutLayer`, a `select!`) leaves its connection mid-exchange. sqlx
    // pings the connection on release, which flushes the pending bytes and
    // drains to `ReadyForQuery`, and closes it if that surfaces an error, so
    // the pool never hands out a connection that is out of step.
    common::db::connect(db_url, &PgPoolSpec::new(max_connections, 5), session).await
}

/// Session-level advisory lock key that serializes boot-time schema work
/// (schema sync, migrations, seeding, plugin-login provisioning) across every
/// server booting against the same database. The value is arbitrary but fixed
/// ("broccoli" in ASCII); it only has to be unique among this database's
/// advisory locks.
pub const BOOT_LOCK_KEY: i64 = 0x6272_6f63_636f_6c69;

/// One dedicated database session holding [`BOOT_LOCK_KEY`] for the duration
/// of boot-time schema work.
///
/// Servers booting together on a fresh database therefore run schema sync,
/// migrations and check-then-insert seeding one at a time, instead of racing
/// each other's DDL and inserting duplicate seed rows.
///
/// The session is a single never-recycled connection (so the session-level
/// lock and the work run on the same backend) with no `statement_timeout`
/// (DDL on a large database legitimately runs long). If this
/// process dies while holding the lock, Postgres releases it when the session
/// ends; the server-side keepalive in [`common::db`] bounds that to about a
/// minute even when the host vanishes without closing the socket.
pub struct BootSession {
    db: DatabaseConnection,
}

impl BootSession {
    /// Open the session and take the boot lock, waiting for a peer that is
    /// booting to finish first.
    pub async fn acquire(db_url: &str) -> Result<Self, DbErr> {
        let db = common::db::connect(
            db_url,
            &PgPoolSpec::single_session(),
            &PgSession::unbounded(APP_NAME),
        )
        .await?;
        let boot = Self { db };
        // Override any role- or database-level default too. A plain `SET` (not
        // a startup option) also works through a PgBouncer in session mode.
        boot.db
            .execute_unprepared("SET statement_timeout = 0")
            .await?;
        let held = boot
            .db
            .query_one_raw(boot.lock_stmt("SELECT pg_try_advisory_lock($1) AS held"))
            .await?
            .map(|r| r.try_get::<bool>("", "held"))
            .transpose()?
            .unwrap_or(false);
        if !held {
            tracing::info!(
                lock_key = BOOT_LOCK_KEY,
                "Another server is running boot-time schema work; waiting for it to finish"
            );
            boot.db
                .execute_raw(boot.lock_stmt("SELECT pg_advisory_lock($1)"))
                .await?;
        }
        tracing::info!("Acquired the boot lock");
        Ok(boot)
    }

    fn lock_stmt(&self, sql: &str) -> sea_orm::Statement {
        sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            sql,
            [BOOT_LOCK_KEY.into()],
        )
    }

    /// The locked session, for seeding and provisioning.
    pub fn conn(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Schema sync, versioned migrations and the claim notification trigger.
    pub async fn migrate(&self) -> Result<(), DbErr> {
        let db = &self.db;
        crate::migration::refuse_unknown_migrations(db).await?;
        // Create the core tables from the entity definitions, then apply the
        // schema they cannot express (see `crate::migration`).
        db.get_schema_registry("server::entity::*").sync(db).await?;
        Migrator::up(db, None).await?;
        crate::services::judging::install_claim_notify(db).await
    }

    /// Release the lock and close the session. Dropping a `BootSession`
    /// without calling this also releases the lock, once the pool closes.
    pub async fn release(self) -> Result<(), DbErr> {
        self.db
            .execute_raw(self.lock_stmt("SELECT pg_advisory_unlock($1)"))
            .await?;
        self.db.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_session() -> PgSession {
        app_session(&crate::config::DatabaseConfig::default())
    }

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

    async fn fresh_postgres() -> (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        String,
    ) {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        (container, url)
    }

    /// Drive `fut` for at most `polls` polls, yielding between them, then drop
    /// it wherever it stopped.
    async fn abandon_after<F: std::future::Future>(fut: F, polls: u64) {
        let mut fut = std::pin::pin!(fut);
        for _ in 0..polls {
            if futures::poll!(fut.as_mut()).is_ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Statements dropped at arbitrary points (before the write, mid-flush,
    /// mid-response, mid-row-stream, and before a server-side 22021 arrives)
    /// never leave a pooled connection out of step: every statement that runs
    /// to completion afterwards gets exactly its own result, never an earlier
    /// statement's rows or error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_statements_never_leave_a_pooled_connection_out_of_step() {
        use sea_orm::{DbBackend, Statement};

        let (_c, url) = fresh_postgres().await;
        let db = connect_main_pool(&url, 4, &test_session())
            .await
            .expect("pool");
        let victims = [
            // Fails on the server with 22021 "invalid byte sequence for
            // encoding \"UTF8\": 0x00" after the client has gone.
            "SELECT pg_sleep(0.001), convert_from('\\x610062'::bytea, 'UTF8')",
            "SELECT pg_sleep(0.002), $1::text",
            "SELECT repeat($1, 2000) FROM generate_series(1, 300)",
        ];
        let workers: Vec<_> = (0..16u64)
            .map(|worker| {
                let db = db.clone();
                tokio::spawn(async move {
                    let mut seed = worker.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                    for i in 0..150u64 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let sql = victims[(seed % 3) as usize];
                        let values: Vec<sea_orm::Value> = if sql.contains("$1") {
                            vec!["x".into()]
                        } else {
                            vec![]
                        };
                        let victim =
                            Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
                        abandon_after(db.query_all_raw(victim), (seed >> 8) % 16).await;

                        let token = format!("{worker}-{i}");
                        let row = db
                            .query_one_raw(Statement::from_sql_and_values(
                                DbBackend::Postgres,
                                "SELECT $1::text AS v, $2::bigint AS n",
                                [token.clone().into(), (i as i64).into()],
                            ))
                            .await
                            .unwrap_or_else(|e| {
                                panic!("clean statement after a cancelled one failed: {e}")
                            })
                            .expect("one row");
                        assert_eq!(row.try_get::<String>("", "v").unwrap(), token);
                        assert_eq!(row.try_get::<i64>("", "n").unwrap(), i as i64);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.await.expect("worker");
        }
    }

    /// The server's boot sequence as `ServerRuntime::build` runs it.
    async fn boot_like_the_server(url: &str) -> Result<(), DbErr> {
        let boot = BootSession::acquire(url).await?;
        boot.migrate().await?;
        crate::seed::seed_role_permissions(boot.conn()).await?;
        let bootstrap = crate::config::BootstrapConfig {
            admin_username: "admin".into(),
            admin_password: "pw".into(),
            ..Default::default()
        };
        crate::seed::ensure_bootstrap_admin(boot.conn(), &bootstrap).await?;
        boot.release().await
    }

    /// The boot-time skew check reads both shared clocks for real; on one host
    /// they agree with ours, so it passes even with a strict bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clock_skew_check_reads_postgres_and_redis() {
        use testcontainers::runners::AsyncRunner;
        let (_pg, url) = fresh_postgres().await;
        let redis = common::test_util::redis_image()
            .start()
            .await
            .expect("start redis");
        let port = redis.get_host_port_ipv4(6379).await.expect("redis port");
        let client = redis::Client::open(format!("redis://127.0.0.1:{port}")).unwrap();
        let db = connect_main_pool(&url, 1, &test_session())
            .await
            .expect("pool");
        let strict = common::clock::ClockConfig {
            warn_skew_ms: 1_000,
            max_skew_ms: 1_000,
        };
        common::clock::check_clock_skew(&db, Some(&client), &strict)
            .await
            .expect("same-host clocks agree");
    }

    /// The boot session is unbounded even when the role carries a default
    /// `statement_timeout` (it sets 0 explicitly instead of relying on startup
    /// options, which a PgBouncer may strip).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_session_overrides_a_role_level_statement_timeout() {
        let (_c, url) = fresh_postgres().await;
        let admin = connect_main_pool(&url, 1, &test_session())
            .await
            .expect("pool");
        admin
            .execute_unprepared("ALTER ROLE postgres SET statement_timeout = '1s'")
            .await
            .expect("role default");
        admin.close().await.expect("close");
        let boot = BootSession::acquire(&url).await.expect("acquire");
        boot.conn()
            .execute_unprepared("SELECT pg_sleep(1.5)")
            .await
            .expect("boot DDL is not cut off by the role default");
        boot.release().await.expect("release");
    }

    /// A second booter blocks on the boot lock until the first releases it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_lock_serializes_booters() {
        let (_c, url) = fresh_postgres().await;
        let first = BootSession::acquire(&url).await.expect("first acquire");
        let second_url = url.clone();
        let mut second = tokio::spawn(async move { BootSession::acquire(&second_url).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut second)
                .await
                .is_err(),
            "second booter must wait while the first holds the lock"
        );
        first.release().await.expect("release");
        let second = tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .expect("second booter proceeds once the lock is released")
            .expect("join")
            .expect("second acquire");
        second.release().await.expect("release");
    }

    /// Servers booting together on a fresh database all succeed, and exactly
    /// one bootstrap admin exists, guarded by the unique username index.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_first_boots_converge() {
        let (_c, url) = fresh_postgres().await;
        let boots: Vec<_> = (0..4)
            .map(|_| {
                let url = url.clone();
                tokio::spawn(async move { boot_like_the_server(&url).await })
            })
            .collect();
        for b in boots {
            b.await
                .expect("join")
                .expect("every concurrent boot succeeds");
        }
        let db = connect_main_pool(&url, 1, &test_session())
            .await
            .expect("pool");
        let row = db
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM \"user\" WHERE username = 'admin')::bigint AS admins, \
                 EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_user_username_active') AS idx",
            ))
            .await
            .expect("query")
            .expect("row");
        assert_eq!(row.try_get::<i64>("", "admins").unwrap(), 1);
        assert!(row.try_get::<bool>("", "idx").unwrap());
    }

    /// A database that records a migration this build does not define is
    /// refused before schema sync touches it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_refuses_a_database_from_another_build_untouched() {
        let (_c, url) = fresh_postgres().await;
        let boot = BootSession::acquire(&url).await.expect("acquire");
        boot.conn()
            .execute_unprepared(
                "CREATE TABLE seaql_migrations (version varchar PRIMARY KEY, applied_at bigint NOT NULL); \
                 INSERT INTO seaql_migrations VALUES ('m0099_from_another_build', 0)",
            )
            .await
            .expect("record a foreign migration");
        let err = boot
            .migrate()
            .await
            .expect_err("a foreign migration must refuse boot");
        let msg = err.to_string();
        assert!(msg.contains("m0099_from_another_build"), "{msg}");
        assert!(msg.contains("empty database"), "{msg}");
        let tables = boot
            .conn()
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS n FROM pg_tables WHERE schemaname = 'public'",
            ))
            .await
            .expect("count tables")
            .expect("row")
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(tables, 1, "only seaql_migrations exists; sync never ran");
        boot.release().await.expect("release");
    }

    /// A reboot leaves every index exactly as the first boot built it: schema
    /// sync neither drops the migration's partial unique indexes nor rebuilds
    /// anything, so the invariants they enforce never lapse.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reboot_keeps_every_index() {
        let (_c, url) = fresh_postgres().await;
        async fn indexes(url: &str) -> Vec<(String, i64, String)> {
            let db = connect_main_pool(url, 1, &test_session())
                .await
                .expect("pool");
            let rows = db
                .query_all_raw(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT indexname::text AS name, \
                     (quote_ident(schemaname) || '.' || quote_ident(indexname))::regclass::oid::bigint AS oid, \
                     indexdef FROM pg_indexes WHERE schemaname = 'public' ORDER BY indexname",
                ))
                .await
                .expect("list indexes");
            db.close().await.expect("close");
            rows.into_iter()
                .map(|r| {
                    (
                        r.try_get::<String>("", "name").unwrap(),
                        r.try_get::<i64>("", "oid").unwrap(),
                        r.try_get::<String>("", "indexdef").unwrap(),
                    )
                })
                .collect()
        }
        boot_like_the_server(&url).await.expect("first boot");
        let first = indexes(&url).await;
        for partial in [
            "idx_user_username_active",
            "idx_submission_judgement_one_current",
        ] {
            assert!(
                first
                    .iter()
                    .any(|(name, _, def)| name == partial && def.contains(" WHERE ")),
                "{partial} missing after the first boot: {first:?}"
            );
        }
        boot_like_the_server(&url).await.expect("second boot");
        assert_eq!(indexes(&url).await, first);
    }

    /// Every pooled connection carries the shared session settings: a slow
    /// statement is cancelled server-side by `statement_timeout`, and keepalive +
    /// `application_name` are set, on the main AND plugin pools. The migration
    /// session is unbounded, so long DDL is not cut off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pooled_sessions_are_bounded_and_labelled() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let session = app_session(&crate::config::DatabaseConfig {
            statement_timeout_secs: 1,
            ..Default::default()
        });
        let app = init_db_with_max_connections(&url, 2, &session)
            .await
            .expect("migrate + main pool");
        let privileged = init_plugin_db_privileged(&url, 1, &session)
            .await
            .expect("privileged plugin pool");
        let restricted = init_plugin_db(&url, 1, None, &session)
            .await
            .expect("restricted plugin pool");

        async fn show(db: &DatabaseConnection, setting: &str) -> String {
            db.query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("SELECT current_setting('{setting}') AS v"),
            ))
            .await
            .expect("show")
            .expect("row")
            .try_get::<String>("", "v")
            .expect("value")
        }

        for (db, name) in [
            (&app, APP_NAME),
            (&privileged, PLUGIN_APP_NAME),
            (&restricted, PLUGIN_APP_NAME),
        ] {
            assert_eq!(show(db, "statement_timeout").await, "1s");
            assert_eq!(show(db, "application_name").await, name);
            // Keepalives only apply to TCP sessions, which these are.
            assert_eq!(show(db, "tcp_keepalives_idle").await, "30");
            let err = db
                .execute_unprepared("SELECT pg_sleep(3)")
                .await
                .expect_err("statement_timeout must cancel the sleep");
            assert!(
                err.to_string().contains("statement timeout"),
                "unexpected error: {err}"
            );
        }
    }

    /// The plugin role may READ and WRITE (DML) any core table by product
    /// decision, but core schema DDL (`DROP`/`ALTER`)
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
        // read and write grants.
        let app = init_db_with_max_connections(&url, 5, &test_session())
            .await
            .expect("app migration + schema sync");

        // RESTRICTED pool: every connection `SET ROLE broccoli_plugin`.
        let restricted = init_plugin_db(&url, 2, None, &test_session())
            .await
            .expect("restricted plugin pool");
        // PRIVILEGED pool: runs as the app role (no SET ROLE).
        let privileged = init_plugin_db_privileged(&url, 2, &test_session())
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

        let app = init_db_with_max_connections(&url, 5, &test_session())
            .await
            .expect("app migration + schema sync");

        // Provision the dedicated login using the privileged app connection.
        let login = provision_restricted_plugin_login(&app)
            .await
            .expect("app role (superuser here) can provision the dedicated login");
        assert_eq!(login.username, PLUGIN_DB_LOGIN_ROLE);

        // Single-connection restricted pool authenticating as the dedicated role.
        let restricted = init_plugin_db(&url, 1, Some(login), &test_session())
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

        let _app = init_db_with_max_connections(&url, 5, &test_session())
            .await
            .expect("app migration + schema sync");

        // A login that can never authenticate: the role does not exist.
        let unusable = PluginLoginOverride {
            username: "no_such_plugin_login".to_string(),
            password: "nope".to_string(),
        };
        let resolved = connect_restricted_pool_with_fallback(
            &url,
            url.clone(),
            Some(unusable),
            2,
            &test_session(),
        )
        .await
        .expect("a refused dedicated login must degrade, not error");

        // Degraded to app-role auth, and the returned pool actually works.
        assert_eq!(
            resolved.auth,
            RestrictedPluginAuth::AppRoleDegraded,
            "a refused dedicated login must classify as the app-role degrade path so \
             the caller fails raw plugin SQL closed"
        );
        resolved
            .pool
            .execute_unprepared("SELECT 1")
            .await
            .expect("the degraded app-role pool must be usable");
    }

    /// By product decision plugins may `SELECT` every table, including the
    /// sensitive ones (credential columns are argon2-hashed at rest; contest
    /// admins are trusted). Drives the REAL migration + REAL restricted pool
    /// against a live Postgres. This test covers reads only;
    /// `broccoli_plugin_can_read_write_core_but_not_ddl_it_and_owns_its_tables`
    /// covers that the plugin role may WRITE (DML) core tables while core DDL
    /// stays owner-only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broccoli_plugin_can_read_every_table() {
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

        let _app = init_db_with_max_connections(&url, 5, &test_session())
            .await
            .expect("app migration + schema sync");
        let restricted = init_plugin_db(&url, 2, None, &test_session())
            .await
            .expect("restricted plugin pool");

        // Sensitive tables are SELECT-able by the plugin role.
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
                .unwrap_or_else(|e| panic!("SELECT from {table} must be allowed, got: {e}"));
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
