//! The single builder for every Postgres pool the server and the worker open.
//!
//! Every pool goes through [`pg_connect_options`] (per-connection session
//! settings) and [`PgPoolSpec`] (pool shape), so a robustness setting added here
//! reaches all of them at once instead of drifting across hand-rolled
//! `ConnectOptions` blocks.
//!
//! What the session settings buy, and what they cannot:
//!
//! - `statement_timeout` makes the *database* abort a statement that runs too
//!   long, so a slow query or a lock pile-up surfaces as an error instead of a
//!   stuck caller. Boot DDL opts out (see [`PgSession::unbounded`]).
//! - `tcp_keepalives_*` make the *database* notice a client that vanished
//!   without a FIN (power loss, cable pull) within about a minute, which frees
//!   its connection slot and every lock it held, including the boot advisory
//!   lock.
//! - Neither helps the *client* when the database host itself disappears while
//!   a query is in flight: sqlx exposes no socket options, so the client read
//!   waits for the kernel's retransmission budget. Callers that must not wedge
//!   (background loops, the worker's task cache) bound their calls with
//!   [`crate::deadline::bounded`] instead.

use std::time::Duration;

use sea_orm::sqlx::ConnectOptions as _;
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{DbErr, RuntimeErr, SqlxPostgresConnector};

mod handle;
pub use handle::{DatabaseConnection, DatabaseTransaction};

/// Default server-side `statement_timeout`. Matches the HTTP `TimeoutLayer`: a
/// handler's statement cannot usefully outlive the request that issued it.
pub const DEFAULT_STATEMENT_TIMEOUT_SECS: u64 = 60;

/// Serde default for the `database.statement_timeout_secs` key of both binaries.
pub fn default_statement_timeout_secs() -> u64 {
    DEFAULT_STATEMENT_TIMEOUT_SECS
}

/// Serde default for the `database.tcp_keepalives` key of both binaries.
pub fn default_tcp_keepalives() -> bool {
    true
}

/// Server-side keepalive: first probe after 30 s idle, then every 10 s, give up
/// after 3 misses. A vanished client is reaped in about 60 s.
const TCP_KEEPALIVES: [(&str, &str); 3] = [
    ("tcp_keepalives_idle", "30"),
    ("tcp_keepalives_interval", "10"),
    ("tcp_keepalives_count", "3"),
];

/// Every session setting the host may enforce through startup `options`. A
/// session-level `SET` of one of these from untrusted SQL (plugin raw SQL)
/// would outlive the call on a pooled connection and silently lift the bound
/// for whoever reuses it, so the plugin SQL guard refuses it (see
/// `server::host_funcs::sql`).
pub const ENFORCED_SESSION_SETTINGS: [&str; 4] = [
    "statement_timeout",
    TCP_KEEPALIVES[0].0,
    TCP_KEEPALIVES[1].0,
    TCP_KEEPALIVES[2].0,
];

/// Per-connection session settings applied at connect time (startup
/// `options`), so they hold for every physical connection in the pool,
/// including ones opened after a reconnect.
///
/// Only enabled settings are sent: with `statement_timeout_secs = 0` and
/// `tcp_keepalives = false` no startup `options` parameter is sent at all,
/// which is what a PgBouncer in front of Postgres needs unless it lists
/// `options` in `ignore_startup_parameters` (configure the bounds on the
/// Postgres role instead in that setup).
#[derive(Debug, Clone)]
pub struct PgSession {
    /// Shown in `pg_stat_activity.application_name`, e.g. `broccoli-server`.
    pub application_name: &'static str,
    /// `None` sends no timeout (the server or role default applies).
    pub statement_timeout: Option<Duration>,
    /// Send the server-side keepalive settings.
    pub tcp_keepalives: bool,
}

impl PgSession {
    /// A session bounded by the configured `statement_timeout_secs` (`0`
    /// sends none), with keepalives when `tcp_keepalives` is set.
    pub fn new(
        application_name: &'static str,
        statement_timeout_secs: u64,
        tcp_keepalives: bool,
    ) -> Self {
        Self {
            application_name,
            statement_timeout: (statement_timeout_secs > 0)
                .then(|| Duration::from_secs(statement_timeout_secs)),
            tcp_keepalives,
        }
    }

    /// A session that sends no statement timeout, for known long statements
    /// such as migrations and boot backfills. Callers that must
    /// also override a role-level default run `SET statement_timeout = 0`
    /// themselves (see the server's boot session).
    pub fn unbounded(application_name: &'static str) -> Self {
        Self {
            application_name,
            statement_timeout: None,
            tcp_keepalives: true,
        }
    }

    /// The startup `options` to send; empty when every bound is disabled.
    fn startup_options(&self) -> Vec<(String, String)> {
        let keepalives = TCP_KEEPALIVES
            .iter()
            .filter(|_| self.tcp_keepalives)
            .map(|(k, v)| (k.to_string(), v.to_string()));
        let timeout = self
            .statement_timeout
            .map(|d| ("statement_timeout".to_string(), d.as_millis().to_string()));
        keepalives.chain(timeout).collect()
    }
}

/// Parse `url` and apply the session settings. Statement logging is disabled
/// (sqlx would otherwise log every statement at INFO).
pub fn pg_connect_options(url: &str, session: &PgSession) -> Result<PgConnectOptions, DbErr> {
    let mut options = url
        .parse::<PgConnectOptions>()
        .map_err(sqlx_err)?
        .disable_statement_logging()
        .application_name(session.application_name);
    // `options()` creates the startup parameter even for an empty iterator, so
    // only call it when there is something to send.
    let startup = session.startup_options();
    if !startup.is_empty() {
        options = options.options(startup);
    }
    Ok(options)
}

/// Pool shape. [`PgPoolSpec::new`] carries the defaults every long-lived pool
/// in the repo uses; tweak fields for special pools.
#[derive(Debug, Clone)]
pub struct PgPoolSpec {
    pub max_connections: u32,
    pub min_connections: u32,
    /// Bounds both connecting and waiting for a free connection.
    pub acquire_timeout: Duration,
    pub idle_timeout: Option<Duration>,
    pub max_lifetime: Option<Duration>,
    /// Ping a connection before handing it out, so a connection left desynced
    /// by a cancelled call (or broken by a DB restart) is discarded instead of
    /// failing the next statement.
    pub test_before_acquire: bool,
}

impl PgPoolSpec {
    pub fn new(max_connections: u32, min_connections: u32) -> Self {
        Self {
            max_connections,
            min_connections: min_connections.min(max_connections),
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Some(Duration::from_secs(600)),
            max_lifetime: Some(Duration::from_secs(1800)),
            test_before_acquire: true,
        }
    }

    /// One connection that is never recycled: the session (and any session
    /// level lock or setting) lives exactly as long as the pool.
    pub fn single_session() -> Self {
        Self {
            idle_timeout: None,
            max_lifetime: None,
            ..Self::new(1, 1)
        }
    }

    pub fn pool_options(&self) -> PgPoolOptions {
        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(self.min_connections)
            .acquire_timeout(self.acquire_timeout)
            .idle_timeout(self.idle_timeout)
            .max_lifetime(self.max_lifetime)
            .test_before_acquire(self.test_before_acquire)
    }
}

/// Wait until Postgres at `url` accepts a connection (see [`crate::boot`]).
/// One direct connection per attempt surfaces the real cause (refused,
/// unknown host, bad password) instead of a pool's bare "timed out".
pub async fn wait_until_reachable(url: &str, session: &PgSession) -> anyhow::Result<()> {
    use crate::boot::{Unreachable, wait_for};
    use sea_orm::sqlx::Connection;

    let options = pg_connect_options(url, session)?;
    let target = crate::redact::redact_url(url);
    wait_for("the database", &target, || async {
        match tokio::time::timeout(CONNECT_ATTEMPT, options.connect()).await {
            Ok(Ok(conn)) => {
                let _ = conn.close().await;
                Ok(())
            }
            Ok(Err(e)) if connect_error_is_fatal(&e) => Err(Unreachable::Fatal(e.to_string())),
            Ok(Err(e)) => Err(Unreachable::Transient(e.to_string())),
            Err(_) => Err(Unreachable::Transient(format!(
                "no answer within {CONNECT_ATTEMPT:?}"
            ))),
        }
    })
    .await
}

/// How long one connection attempt at boot may take.
const CONNECT_ATTEMPT: Duration = Duration::from_secs(5);

/// Whether a connection error is a refusal no retry fixes: a malformed URL,
/// rejected credentials (SQLSTATE class 28) or a database that does not exist
/// (3D000). Anything else (refused, reset, timed out, too many clients) is
/// the server not being ready yet.
fn connect_error_is_fatal(e: &sea_orm::sqlx::Error) -> bool {
    use sea_orm::sqlx::Error;
    match e {
        Error::Configuration(_) => true,
        Error::Database(db) => db
            .code()
            .is_some_and(|code| code.starts_with("28") || code == "3D000"),
        _ => false,
    }
}

/// Open a pool with `spec` and `session` behind a [`DatabaseConnection`].
pub async fn connect(
    url: &str,
    spec: &PgPoolSpec,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    connect_with(spec.pool_options(), pg_connect_options(url, session)?).await
}

/// Open a pool from pre-built options (for pools that add an `after_connect`
/// hook or override credentials) behind a [`DatabaseConnection`].
pub async fn connect_with(
    pool_options: PgPoolOptions,
    connect_options: PgConnectOptions,
) -> Result<DatabaseConnection, DbErr> {
    let pool = pool_options
        .connect_with(connect_options)
        .await
        .map_err(sqlx_err)?;
    Ok(SqlxPostgresConnector::from_sqlx_postgres_pool(pool).into())
}

/// [`connect_with`], but the pool opens no connection until its first use.
pub fn connect_lazy_with(
    pool_options: PgPoolOptions,
    connect_options: PgConnectOptions,
) -> DatabaseConnection {
    SqlxPostgresConnector::from_sqlx_postgres_pool(pool_options.connect_lazy_with(connect_options))
        .into()
}

fn sqlx_err(e: sea_orm::sqlx::Error) -> DbErr {
    DbErr::Conn(RuntimeErr::SqlxError(e.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startup_options(o: &PgConnectOptions) -> String {
        o.get_options().unwrap_or_default().to_string()
    }

    #[test]
    fn session_settings_reach_the_startup_options() {
        let o = pg_connect_options(
            "postgres://u:p@db:5432/x",
            &PgSession::new("broccoli-test", 60, true),
        )
        .unwrap();
        let s = startup_options(&o);
        assert!(s.contains("statement_timeout=60000"), "{s}");
        assert!(s.contains("tcp_keepalives_idle=30"), "{s}");
        assert!(s.contains("tcp_keepalives_count=3"), "{s}");
        assert_eq!(o.get_application_name(), Some("broccoli-test"));
    }

    #[test]
    fn disabled_bounds_are_not_sent() {
        let o = pg_connect_options("postgres://db/x", &PgSession::new("t", 0, true)).unwrap();
        let s = startup_options(&o);
        assert!(!s.contains("statement_timeout"), "{s}");
        assert!(s.contains("tcp_keepalives_idle=30"), "{s}");
        let o = pg_connect_options("postgres://db/x", &PgSession::unbounded("t")).unwrap();
        assert!(!startup_options(&o).contains("statement_timeout"));
    }

    /// With every bound disabled no `options` startup parameter is sent, so a
    /// PgBouncer that does not ignore it accepts the connection.
    #[test]
    fn nothing_enabled_sends_no_options_parameter() {
        let o = pg_connect_options("postgres://db/x", &PgSession::new("t", 0, false)).unwrap();
        assert_eq!(o.get_options(), None);
    }

    #[test]
    fn url_options_are_kept() {
        let o = pg_connect_options(
            "postgres://db/x?options=-c%20search_path%3Dfoo",
            &PgSession::new("t", 5, true),
        )
        .unwrap();
        let s = startup_options(&o);
        assert!(s.contains("search_path=foo"), "{s}");
        assert!(s.contains("statement_timeout=5000"), "{s}");
    }
}
