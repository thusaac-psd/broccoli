use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use extism::host_fn;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tracing::error;
use uuid::Uuid;

/// A plugin-opened DB transaction plus the bookkeeping needed to keep it from
/// leaking or being hijacked: the owning plugin (txn ids are only usable by the
/// plugin that opened them) and the last time the plugin touched it (drives the
/// idle-TTL reaper).
#[derive(Debug)]
pub struct PluginTransaction {
    txn: DatabaseTransaction,
    plugin_id: String,
    last_used: Instant,
}

pub type TransactionMap = Arc<StdMutex<HashMap<String, PluginTransaction>>>;

/// How long an open plugin transaction may sit untouched before the reaper
/// rolls it back and returns its pooled connection. Every db_query_in /
/// db_execute_in refreshes the timestamp, so a live transaction is only at
/// risk if the guest goes silent for the whole window. Sized at 2x the default
/// plugin `call_timeout_secs` (300s): a transaction legitimately held across a
/// call can idle for at most one call timeout, so anything past this is an
/// orphan from a trapped/killed guest (WASM OOM, host timeout, panic) whose
/// commit/rollback will never arrive.
const TXN_IDLE_TTL: Duration = Duration::from_secs(600);

/// How often each per-transaction reaper task re-checks its transaction.
const TXN_REAP_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum transactions a single plugin may hold open concurrently. Bounds the
/// damage between reaper sweeps: without a cap, one misbehaving plugin that
/// traps after `db_begin` in a loop can check out the entire plugin DB pool
/// (20 connections by default) and starve every other plugin's DB access.
const MAX_OPEN_TXNS_PER_PLUGIN: usize = 8;

/// Watchdog for one plugin transaction: once the transaction has been idle for
/// [`TXN_IDLE_TTL`] it is removed from the map and rolled back, returning its
/// pooled connection. This is the only cleanup path when a guest traps between
/// `db_begin` and `db_commit`/`db_rollback` - the host_fn-side commit/rollback
/// removals never run for a dead guest, and a leaked `DatabaseTransaction`
/// pins a plugin-pool connection forever. The task exits as soon as the
/// transaction is committed/rolled back (entry gone) or reaped.
fn spawn_txn_reaper(txn_map: TransactionMap, txn_id: String) {
    spawn_txn_reaper_with(txn_map, txn_id, TXN_IDLE_TTL, TXN_REAP_INTERVAL)
}

/// Inner helper split out from [`spawn_txn_reaper`] so the reaping behaviour
/// is testable with short timings instead of the production 10-minute TTL.
fn spawn_txn_reaper_with(
    txn_map: TransactionMap,
    txn_id: String,
    idle_ttl: Duration,
    reap_interval: Duration,
) {
    tokio::runtime::Handle::current().spawn(async move {
        loop {
            tokio::time::sleep(reap_interval).await;
            let orphaned = {
                // try_lock: db_query_in/db_execute_in hold this mutex across
                // their blocking DB call, so a contended lock means some
                // transaction is actively in use. Skip the tick instead of
                // parking a runtime worker on a std mutex.
                let mut guard = match txn_map.try_lock() {
                    Ok(guard) => guard,
                    // A host_fn thread panicked while holding the lock; the
                    // transactions still need reaping, and the map itself is
                    // just a HashMap that is valid at every await-free point.
                    Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                    Err(TryLockError::WouldBlock) => continue,
                };
                match guard.get(&txn_id) {
                    // Committed or rolled back through the normal path.
                    None => return,
                    Some(entry) if entry.last_used.elapsed() < idle_ttl => None,
                    Some(_) => guard.remove(&txn_id),
                }
            };
            if let Some(entry) = orphaned {
                tracing::warn!(
                    plugin_id = %entry.plugin_id,
                    txn_id = %txn_id,
                    idle_ttl_secs = idle_ttl.as_secs(),
                    "Rolling back orphaned plugin DB transaction (guest never committed or rolled back; likely trapped)"
                );
                if let Err(e) = entry.txn.rollback().await {
                    error!(txn_id = %txn_id, "Failed to roll back orphaned plugin DB transaction: {}", e);
                }
                return;
            }
        }
    });
}

/// Database URL used to open a FRESH one-shot connection as the guaranteed
/// last-resort fallback when pool retries are all exhausted by poisoned-
/// connection 0x00 errors. Set once at startup (see runtime.rs). A brand-new
/// connection has no prior in-flight query, so its write buffer / protocol
/// state are clean by construction - it CANNOT be poisoned - making the insert
/// succeed deterministically (modulo a genuine DB failure).
static PLUGIN_DB_FALLBACK_URL: OnceLock<String> = OnceLock::new();

/// Register the URL for the fresh-connection fallback. Idempotent.
pub fn set_plugin_db_fallback_url(url: String) {
    let _ = PLUGIN_DB_FALLBACK_URL.set(url);
}

/// Connection parameters for the RESTRICTED fresh-connection fallback. When set,
/// the restricted 0x00 fallback authenticates as the dedicated non-privileged
/// login role (see [`crate::database::provision_restricted_plugin_login`]),
/// exactly like the restricted pool, instead of the privileged app role - so an
/// escape that survives the SQL text guard lands on no privileges on THIS path
/// too, keeping the invariant "every restricted path authenticates as a
/// non-privileged login" whole. Its `url` may differ from
/// [`PLUGIN_DB_FALLBACK_URL`] (e.g. a DBA-managed `database.plugin_url`). When
/// unset, the restricted fallback keeps app-role auth plus the after-connect
/// `SET ROLE broccoli_plugin` downshift (backward compatible).
static PLUGIN_DB_RESTRICTED_FALLBACK: OnceLock<RestrictedFallback> = OnceLock::new();

struct RestrictedFallback {
    url: String,
    login: Option<crate::database::PluginLoginOverride>,
}

/// Register the restricted fresh-connection fallback target. Idempotent.
pub fn set_plugin_db_restricted_fallback(
    url: String,
    login: Option<crate::database::PluginLoginOverride>,
) {
    let _ = PLUGIN_DB_RESTRICTED_FALLBACK.set(RestrictedFallback { url, login });
}

/// Set once, at startup, when the restricted plugin SQL pool could only degrade
/// to privileged app-role auth (no dedicated login role, no operator-managed
/// `database.plugin_url`). In that state the SQL text guard would be the SOLE
/// barrier against a role escape, and the guard is best-effort lexer defense, not
/// an engine-level boundary -- so raw plugin SQL (`host.db.*`) is refused outright
/// (fail CLOSED) rather than run as the privileged application role. The
/// server-owned STRUCTURED host functions (submission/storage/config) are
/// unaffected: they run on the separate privileged pool by design, not on
/// plugin-authored SQL. See [`crate::database::RestrictedPluginAuth`].
static RAW_PLUGIN_SQL_DISABLED: OnceLock<bool> = OnceLock::new();

/// Fail closed: disable the raw `sql` capability (`host.db.*`) for this process.
/// Idempotent; call at startup when the restricted pool degraded to app-role auth.
pub fn disable_raw_plugin_sql() {
    let _ = RAW_PLUGIN_SQL_DISABLED.set(true);
}

fn raw_plugin_sql_disabled() -> bool {
    RAW_PLUGIN_SQL_DISABLED.get().copied().unwrap_or(false)
}

/// Gate for every raw plugin SQL / transaction entry point. Pure in `disabled`
/// so it is unit-testable without touching the process-global `OnceLock`.
fn raw_sql_gate(disabled: bool, plugin_id: &str) -> Result<(), extism::Error> {
    if disabled {
        tracing::warn!(
            plugin_id,
            "Refused a raw plugin SQL call: the restricted database login could not \
             be provisioned, so host.db.* is disabled (fail-closed) rather than run \
             as the privileged application role"
        );
        return Err(extism::Error::msg(
            "raw plugin SQL (host.db.*) is disabled on this server: the restricted, \
             least-privilege database login could not be provisioned, so plugin SQL \
             is refused rather than executed as the privileged application role. \
             Grant the application role CREATEROLE, or set database.plugin_url to a \
             dedicated non-privileged role, to enable the raw SQL capability. \
             (Structured host functions are unaffected.)",
        ));
    }
    Ok(())
}

/// Resolve the (url, login-override) a fresh fallback connection should use for
/// `role`. The restricted role prefers the dedicated-login target when set; the
/// privileged role always uses the app-role fallback URL.
fn fallback_target(
    role: FallbackRole,
) -> Result<(String, Option<crate::database::PluginLoginOverride>), sea_orm::DbErr> {
    if role == FallbackRole::Restricted
        && let Some(rf) = PLUGIN_DB_RESTRICTED_FALLBACK.get()
    {
        return Ok((rf.url.clone(), rf.login.clone()));
    }
    let url = PLUGIN_DB_FALLBACK_URL
        .get()
        .ok_or_else(|| sea_orm::DbErr::Custom("plugin DB fallback URL not set".to_string()))?
        .clone();
    Ok((url, None))
}

/// Open a fresh single-connection pool, applying a login override when present so
/// the connection can authenticate as the dedicated non-privileged login role
/// rather than the app role. Overriding on `PgConnectOptions` (not by rewriting
/// the URL) avoids any credential-escaping concern.
async fn connect_fresh_single(
    url: &str,
    login: Option<&crate::database::PluginLoginOverride>,
) -> Result<DatabaseConnection, sea_orm::DbErr> {
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let mut opts = url
        .parse::<PgConnectOptions>()
        .map_err(|e| sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e.into())))?
        .disable_statement_logging();
    if let Some(login) = login {
        opts = opts.username(&login.username).password(&login.password);
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e.into())))?;
    Ok(sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(
        pool,
    ))
}

/// Which DB role a fresh fallback connection must assume before running its
/// statement. Pooled plugin connections downshift to the `broccoli_plugin` role
/// via `after_connect` (see [`crate::database::init_plugin_db`]), but a fresh
/// fallback connection is opened OUTSIDE that pool and would otherwise default to
/// the privileged app role. The raw `sql` path must re-apply the plugin role so
/// riding the 0x00 fallback cannot slip a statement past the role's boundary
/// (schema DDL on core, `plugin_login_secret`) by running as the app role; the
/// server-owned structured `host.submission.*` path is privileged by design and
/// keeps the app role.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FallbackRole {
    /// Raw plugin SQL: `SET ROLE broccoli_plugin`, matching the restricted pool.
    Restricted,
    /// Server-owned structured writes (`host.submission.*`): keep the app role.
    Privileged,
}

/// Re-apply the restricted plugin role on a freshly opened fallback connection.
/// A no-op for [`FallbackRole::Privileged`]. The fallback pool holds exactly one
/// connection (`max`/`min` = 1), so this `SET ROLE` and the statement that
/// follows provably run on the same physical connection.
async fn apply_fallback_role(
    conn: &DatabaseConnection,
    role: FallbackRole,
) -> Result<(), sea_orm::DbErr> {
    if role == FallbackRole::Restricted {
        conn.execute_raw(Statement::from_string(
            DbBackend::Postgres,
            format!("SET ROLE {}", crate::database::PLUGIN_DB_ROLE),
        ))
        .await?;
    }
    Ok(())
}

/// Open a fresh single connection and execute `stmt` on it, then drop it. The
/// connection is brand-new, so it is guaranteed free of stale wire-protocol
/// framing from any prior operation - the guaranteed cure for a poisoned-
/// connection 0x00 when the pool retries are exhausted. `role` decides whether
/// the fresh connection first downshifts to the restricted plugin role, so the
/// fallback carries the SAME privilege as the pool the call came from.
fn execute_on_fresh_connection(
    stmt: Statement,
    role: FallbackRole,
) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
    let (url, login) = fallback_target(role)?;
    execute_on_fresh_connection_url(&url, stmt, role, login.as_ref())
}

/// Inner helper split out from [`execute_on_fresh_connection`] so the
/// fresh-connection behaviour (including the role downshift) is testable against
/// a real database without the process-global fallback `OnceLock`s. `login`, when
/// present, authenticates the fresh connection as the dedicated non-privileged
/// login role instead of the app role.
fn execute_on_fresh_connection_url(
    url: &str,
    stmt: Statement,
    role: FallbackRole,
    login: Option<&crate::database::PluginLoginOverride>,
) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
    tokio::runtime::Handle::current().block_on(async move {
        let fresh = connect_fresh_single(url, login).await?;
        apply_fallback_role(&fresh, role).await?;
        let result = fresh.execute_raw(stmt).await;
        fresh.close().await.ok();
        result
    })
}

/// Read-side counterpart to [`execute_on_fresh_connection`]: open a fresh single
/// connection and run `stmt` (a json_agg-wrapped query) on it, then drop it.
/// `db_query` previously had only the probabilistic pool-retry layer; under
/// sustained poisoning the pool can keep handing back the same desynced
/// connection, so a query could still surface a `0x00` error to the plugin - and
/// a plugin DB error becomes a `SystemError`. A brand-new connection is clean by
/// construction, turning that into a deterministic recovery, matching the
/// guarantee the write path already has. `role` mirrors the calling pool's
/// privilege (restricted for raw `sql`, privileged for `host.submission.*`).
fn query_on_fresh_connection(
    stmt: Statement,
    role: FallbackRole,
) -> Result<Option<sea_orm::QueryResult>, sea_orm::DbErr> {
    let (url, login) = fallback_target(role)?;
    query_on_fresh_connection_url(&url, stmt, role, login.as_ref())
}

/// Inner helper split out from [`query_on_fresh_connection`] so the
/// fresh-connection behaviour is testable against a real database without
/// depending on the process-global fallback `OnceLock`s. `login`, when present,
/// authenticates the fresh connection as the dedicated non-privileged login role.
fn query_on_fresh_connection_url(
    url: &str,
    stmt: Statement,
    role: FallbackRole,
    login: Option<&crate::database::PluginLoginOverride>,
) -> Result<Option<sea_orm::QueryResult>, sea_orm::DbErr> {
    tokio::runtime::Handle::current().block_on(async move {
        let fresh = connect_fresh_single(url, login).await?;
        apply_fallback_role(&fresh, role).await?;
        let result = fresh.query_one_raw(stmt).await;
        fresh.close().await.ok();
        result
    })
}

/// Look up a transaction by id, enforcing that the caller is the plugin that
/// opened it. Txn ids are unguessable UUIDs, but capability boundaries must
/// not rest on unguessability alone: a leaked id (logs, shared guest memory)
/// must not let one plugin commit, roll back, or query inside another
/// plugin's transaction. A foreign txn id is reported exactly like a
/// nonexistent one so plugins cannot probe for other plugins' transactions.
fn lookup_owned_txn<'a>(
    map: &'a mut HashMap<String, PluginTransaction>,
    txn_id: &str,
    plugin_id: &str,
    host_fn: &str,
) -> Result<&'a mut PluginTransaction, extism::Error> {
    match map.get_mut(txn_id) {
        Some(entry) if entry.plugin_id == plugin_id => Ok(entry),
        Some(entry) => {
            tracing::warn!(
                plugin_id,
                owner = %entry.plugin_id,
                txn_id,
                host_fn,
                "Plugin attempted to use a DB transaction it does not own"
            );
            Err(extism::Error::msg(format!(
                "Transaction not found: {txn_id}"
            )))
        }
        None => Err(extism::Error::msg(format!(
            "Transaction not found: {txn_id}"
        ))),
    }
}

#[derive(Serialize)]
pub(super) struct HostDbResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HostDbResponse {
    pub(super) fn ok(data: JsonValue) -> Self {
        Self {
            data: Some(data),
            error: None,
        }
    }
    pub(super) fn err(msg: impl Into<String>) -> Self {
        Self {
            data: None,
            error: Some(msg.into()),
        }
    }
    pub(super) fn to_json_string(&self) -> Result<String, extism::Error> {
        serde_json::to_string(self)
            .map_err(|e| extism::Error::msg(format!("Serialization error: {}", e)))
    }
}

fn sanitize_nul_bytes(s: String, plugin_id: &str, sql: &str) -> String {
    if s.contains('\0') {
        let nul_count = s.matches('\0').count();
        let preview: String = s.chars().take(120).collect();
        let sql_preview: String = sql.chars().take(160).collect();
        tracing::warn!(
            plugin_id,
            nul_count,
            sql = %sql_preview,
            value_preview = %preview,
            "Sanitized NUL bytes from plugin SQL bind value",
        );
        s.replace('\0', "\u{FFFD}")
    } else {
        s
    }
}

/// Reduce a plugin-authored SQL string to just its top-level lexical text so the
/// role guard tokenizes real keywords and `;` statement separators only -- never
/// characters that live inside a string literal, quoted identifier, or comment.
///
/// This must be lexically aware, not a naive scan: a `/*` **inside a string
/// literal** (`SELECT '/*', set_config('role','none',false)`) is not a comment,
/// and a naive comment stripper that entered comment mode there would swallow the
/// rest of the statement and hide the `set_config` -- a fail-OPEN escalation.
/// Likewise a `;` or a `SET ROLE`-looking token inside a string literal is data,
/// not a command, and must not trip the guard (fail-CLOSED false positive).
///
/// Handling per lexical region:
/// - `-- line` and `/* block */` comments (block comments nest, as in Postgres):
///   replaced by a space.
/// - `'string'` (with `''` and, after a standalone `E'` prefix, `\` escapes):
///   replaced by a space -- its body is data, never a command.
/// - `"quoted identifier"`: flattened to a single token (non-identifier chars ->
///   `_`), not dropped. A column named `"set role"` collapses to `set_role`
///   (harmless), but a function called via a quoted name -- `"set_config"(..)` --
///   keeps its `set_config` token so the role-escape check still fires. A
///   `U&"..."` Unicode-escape identifier is fail-CLOSED (rejected): Postgres
///   decodes its `\XXXX` escapes into the real name at parse time, so flattening
///   the literal text would compare `_0073et_config` while Postgres runs
///   `set_config`.
/// - `$tag$ ... $tag$` dollar-quoted bodies: emitted **verbatim** (wrapped in
///   spaces), so a role change smuggled into a `CREATE FUNCTION`/`DO` body is
///   still seen. `$1` positional params and a lone `$` stay ordinary text.
///
/// Returns `Err(())` on any unterminated construct (string, identifier, dollar
/// quote, or block comment); the caller treats that as blocked, i.e. the guard
/// fails CLOSED on lexically ambiguous input rather than guessing.
fn scrub_sql_for_guard(sql: &str) -> Result<String, ()> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            // -- line comment to end of line (newline kept as a token boundary).
            '-' if i + 1 < n && chars[i + 1] == '-' => {
                i += 2;
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                out.push(' ');
            }
            // /* block comment */ -- nestable, matching PostgreSQL.
            '/' if i + 1 < n && chars[i + 1] == '*' => {
                let mut depth = 1usize;
                i += 2;
                while i < n && depth > 0 {
                    if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth > 0 {
                    return Err(());
                }
                out.push(' ');
            }
            // "quoted identifier": flatten to a SINGLE token (every non-ident
            // char, including the "" escape, becomes '_'). A quoted identifier is
            // one identifier, never a multi-word command -- so a column named
            // "set role" must collapse to `set_role` (NOT the SET ROLE keyword
            // pair). But it CAN be a function name: `"set_config"('role',..)` is
            // the built-in set_config called via a quoted identifier, a real role
            // escape. Dropping the body (as a bare space) hid that token and let
            // the call through; keeping it as one token means the downstream
            // set_config match still fires.
            '"' => {
                // U&"..." / u&"..." is a Unicode-escape identifier: Postgres
                // decodes \XXXX / \+XXXXXX into the REAL identifier at parse time,
                // so `U&"\0073et_config"` is the built-in set_config. The literal
                // flatten below compares the pre-decode text (`_0073et_config`),
                // which no longer matches the keyword -- a fail-OPEN role escape.
                // Plugins never need the escape syntax (a UTF-8 identifier can be
                // written directly, `"café"`), so fail CLOSED on the whole class.
                // The `U`/`u` must be its own token abutting `&"` with no gap, per
                // Postgres' rule; that is exactly `chars[i-2..=i-1] == ['u'|'U','&']`
                // with a non-identifier char (or start) before it.
                let unicode_ident = i >= 2
                    && chars[i - 1] == '&'
                    && matches!(chars[i - 2], 'u' | 'U')
                    && !(i >= 3 && (chars[i - 3].is_ascii_alphanumeric() || chars[i - 3] == '_'));
                if unicode_ident {
                    return Err(());
                }
                i += 1;
                let start = i;
                loop {
                    if i >= n {
                        return Err(());
                    }
                    if chars[i] == '"' {
                        if i + 1 < n && chars[i + 1] == '"' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
                for &ch in &chars[start..i] {
                    out.push(if ch.is_ascii_alphanumeric() || ch == '_' {
                        ch
                    } else {
                        '_'
                    });
                }
                out.push(' ');
                i += 1;
            }
            // 'string literal' -- '' always escapes a quote; backslash escapes
            // apply ONLY to a standalone E'...' string (Postgres). The `E`/`e`
            // must be its own token: a string abutting a token that merely ENDS
            // in e (a typed literal like name'..', or LIKE'..') is a plain
            // string, not an escape string. Requiring the char before the e/E to
            // be a non-identifier char avoids both reading past the real closing
            // quote (a ';'-smuggle fail-OPEN) and Err-ing on a valid string that
            // ends in a backslash (a fail-CLOSED false positive).
            '\'' => {
                let prev_is_e = i > 0 && matches!(chars[i - 1], 'e' | 'E');
                let e_is_standalone = prev_is_e
                    && !(i >= 2 && (chars[i - 2].is_ascii_alphanumeric() || chars[i - 2] == '_'));
                let backslash = e_is_standalone;
                i += 1;
                loop {
                    if i >= n {
                        return Err(());
                    }
                    let d = chars[i];
                    if backslash && d == '\\' {
                        i += 2;
                        continue;
                    }
                    if d == '\'' {
                        if i + 1 < n && chars[i + 1] == '\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
            }
            // $tag$ dollar-quoted string $tag$ (tag optional). `$1` positional
            // params and a lone `$` are ordinary text, not a dollar quote.
            '$' => {
                let mut k = i + 1;
                while k < n && (chars[k].is_ascii_alphanumeric() || chars[k] == '_') {
                    k += 1;
                }
                let empty_tag = k == i + 1;
                let tag_valid = empty_tag || !chars[i + 1].is_ascii_digit();
                if k < n && chars[k] == '$' && tag_valid {
                    let delim: Vec<char> = chars[i..=k].to_vec();
                    let dlen = delim.len();
                    let mut p = k + 1;
                    let mut found = false;
                    while p + dlen <= n {
                        if chars[p..p + dlen] == delim[..] {
                            found = true;
                            break;
                        }
                        p += 1;
                    }
                    if !found {
                        return Err(());
                    }
                    // Emit the body verbatim (a role change hidden in a function
                    // body must still be caught); the $tag$ delimiters become
                    // spaces so the body is its own token run.
                    out.push(' ');
                    out.extend(chars[k + 1..p].iter());
                    out.push(' ');
                    i = p + dlen;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Reject a raw plugin-`sql`-capability statement that changes the session role
/// or authorization, or runs an anonymous code block. The restricted pool
/// confines plugins with a session-level `SET ROLE broccoli_plugin`
/// (`database.rs`), but that is REVERSIBLE: without this guard a plugin could
/// `RESET ROLE` / `SET ROLE <app-role>` / `SELECT set_config('role', ...)` /
/// `DISCARD ALL` -- directly, inside a `DO` block, or in a function body it then
/// calls -- to escape back to the privileged login role and gain full write/DDL
/// on core tables and SELECT on PII. We fail closed on any occurrence of these
/// constructs anywhere in the comment-stripped statement. (The fully robust fix
/// is a dedicated unprivileged LOGIN role for the restricted pool, so there is
/// no privileged `session_user` to fall back to; this guard closes the escape
/// on the default single-credential deployment.) Applied ONLY to the raw
/// plugin-authored entry points, never to the server-owned privileged writes.
fn reject_role_escalation(plugin_id: &str, sql: &str) -> Result<(), extism::Error> {
    // Scrub comments and string/identifier bodies so keyword and ';' matching
    // sees only real top-level SQL text (see `scrub_sql_for_guard`). A lexically
    // unbalanced statement scrubs to Err and is treated as blocked -- the guard
    // fails CLOSED rather than guess. Then evaluate each ';'-separated statement
    // independently: splitting on ';' stops a SET ROLE from hiding behind a
    // benign earlier statement (`UPDATE t SET x=1; SET ROLE admin`), and lets a
    // DML SET-clause (`UPDATE ... SET role = $1`, always preceded by UPDATE in
    // its own statement) be told apart from the standalone `SET role = x`
    // role-switch GUC command, which has no preceding UPDATE.
    let blocked = match scrub_sql_for_guard(sql) {
        Ok(scrubbed) => scrubbed
            .to_ascii_lowercase()
            .split(';')
            .any(statement_changes_role),
        Err(()) => true,
    };
    if blocked {
        tracing::warn!(
            plugin_id,
            sql = %sql.chars().take(120).collect::<String>(),
            "Rejected a role/session-authorization change on the restricted plugin SQL path",
        );
        return Err(extism::Error::msg(
            "not permitted on the plugin SQL capability: SET/RESET ROLE, SET SESSION AUTHORIZATION, set_config, DISCARD, and anonymous code blocks are forbidden",
        ));
    }
    Ok(())
}

/// True if a single comment-stripped, lowercased SQL statement changes the
/// session role/authorization, runs an anonymous code block, or defines a
/// routine (whose single-quoted body the lexer cannot see into). Callers pass
/// one ';'-separated statement (see [`reject_role_escalation`]).
fn statement_changes_role(stmt: &str) -> bool {
    // Tokenize into SQL identifier/keyword tokens (split on any char that is not
    // part of an identifier). Token-aware matching is both whitespace-/comment-
    // insensitive (`set_config ('role'..)`, `set/**/role`, tabs) AND precise
    // (`SET role_id = $1` tokenizes as [set, role_id], which is NOT the
    // [set, role] sequence, so ordinary DML on a `role*`/`session*` column is
    // not falsely rejected).
    let tokens: Vec<&str> = stmt
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect();
    // DO runs arbitrary procedural code; DISCARD [ALL] resets the session role --
    // neither is needed by a plugin, so block them as a statement.
    if tokens
        .first()
        .is_some_and(|t| *t == "do" || *t == "discard")
    {
        return true;
    }
    // CREATE [OR REPLACE] FUNCTION/PROCEDURE is the NAMED analogue of the `DO`
    // block above: it defines a routine body the lexer cannot see into. Unlike a
    // `$$..$$` body (emitted verbatim by `scrub_sql_for_guard` so a role change
    // inside is still caught), a SINGLE-QUOTED body -- `AS '.. set_config(''role''
    // ,..) ..'` -- is blanked to one space, hiding a role escape that a later,
    // benign-looking `SELECT f()` then executes (`set_config(.., is_local=>false)`
    // persists to the session). A plugin never needs server-side routines -- its
    // own `CREATE TABLE` stays allowed -- so block the construct outright. The
    // grammar is fixed (`CREATE [OR REPLACE] {FUNCTION|PROCEDURE}`, nothing but
    // the optional `OR REPLACE` between), so the command keyword is the first
    // token after `create` past any `or`/`replace`; a table NAMED `function`
    // (`CREATE TABLE "function"`) keeps `table` as its command word and is not
    // falsely blocked.
    if tokens.first().copied() == Some("create") {
        let command = tokens[1..]
            .iter()
            .copied()
            .find(|&t| t != "or" && t != "replace");
        if matches!(command, Some("function") | Some("procedure")) {
            return true;
        }
    }
    // `set_config('role',..)` / `pg_catalog.set_config(..)` (any spacing), and the
    // underscore `session_authorization` GUC form, anywhere in the statement.
    if tokens
        .iter()
        .any(|t| *t == "set_config" || *t == "session_authorization")
    {
        return true;
    }
    // A SET/RESET changes the role or session authorization when, allowing an
    // optional SESSION/LOCAL scope keyword first, its command is ROLE, ALL
    // (RESET ALL), or the two-word SESSION AUTHORIZATION. Matching the target
    // this way catches the scoped forms `SET LOCAL ROLE`/`SET SESSION ROLE`
    // (SET ROLE needs only role membership, not superuser -- a live escalation)
    // AND `SET LOCAL SESSION AUTHORIZATION` (whose command word SESSION collides
    // with the scope word), while leaving `SET role_id = $1` (target `role_id`,
    // a distinct token) and `SET statement_timeout = 5` untouched.
    tokens.iter().enumerate().any(|(i, &t)| {
        if t != "set" && t != "reset" {
            return false;
        }
        // Window of up to the scope word + a two-word command.
        let after: Vec<&str> = tokens[i + 1..].iter().take(3).copied().collect();
        // `SESSION AUTHORIZATION` as an adjacent pair anywhere in the window
        // (covers plain, `SET LOCAL SESSION AUTHORIZATION`, and the odd
        // `SET SESSION SESSION AUTHORIZATION`).
        let session_auth = after.windows(2).any(|w| w == ["session", "authorization"]);
        // Single-word command target after an optional leading scope word.
        let scoped = matches!(after.first(), Some(&"local") | Some(&"session"));
        let target = if scoped {
            after.get(1).copied()
        } else {
            after.first().copied()
        };
        let changes =
            session_auth || matches!(target, Some("role") | Some("authorization") | Some("all"));
        if !changes {
            return false;
        }
        // A preceding UPDATE (plain UPDATE, INSERT ... ON CONFLICT DO UPDATE, or
        // MERGE ... WHEN MATCHED THEN UPDATE) makes this SET a DML column
        // assignment -- `SET role = value` on a column literally named `role` --
        // not the SET ROLE command. Plugins hold full DML, so that must be
        // allowed; the ';'-split in the caller stops a second statement from
        // hiding behind this UPDATE.
        !tokens[..i].contains(&"update")
    })
}

fn sanitize_sql_text(label: &str, plugin_id: &str, sql: String) -> String {
    if sql.contains('\0') {
        let nul_count = sql.matches('\0').count();
        let mut nul_offsets: Vec<usize> = sql
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == 0)
            .map(|(i, _)| i)
            .take(8)
            .collect();
        nul_offsets.sort_unstable();
        let len = sql.len();
        let preview: String = sql.chars().take(240).collect();
        tracing::warn!(
            plugin_id,
            param = label,
            nul_count,
            len,
            nul_offsets = ?nul_offsets,
            preview = %preview,
            "Sanitized NUL bytes from plugin SQL host_fn parameter",
        );
        sql.replace('\0', "\u{FFFD}")
    } else {
        sql
    }
}

fn json_to_sea_value(v: JsonValue, plugin_id: &str, sql: &str) -> sea_orm::Value {
    match v {
        JsonValue::Null => sea_orm::Value::String(None),
        JsonValue::Bool(b) => sea_orm::Value::Bool(Some(b)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                sea_orm::Value::BigInt(Some(i))
            } else if let Some(f) = n.as_f64() {
                sea_orm::Value::Double(Some(f))
            } else {
                sea_orm::Value::String(Some(n.to_string()))
            }
        }
        JsonValue::String(s) => sea_orm::Value::String(Some(sanitize_nul_bytes(s, plugin_id, sql))),
        v => sea_orm::Value::Json(Some(Box::new(sanitize_json_nul_bytes(v, plugin_id, sql)))),
    }
}

fn sanitize_json_nul_bytes(v: JsonValue, plugin_id: &str, sql: &str) -> JsonValue {
    match v {
        JsonValue::String(s) => JsonValue::String(sanitize_nul_bytes(s, plugin_id, sql)),
        JsonValue::Array(values) => JsonValue::Array(
            values
                .into_iter()
                .map(|value| sanitize_json_nul_bytes(value, plugin_id, sql))
                .collect(),
        ),
        JsonValue::Object(map) => JsonValue::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        sanitize_nul_bytes(key, plugin_id, sql),
                        sanitize_json_nul_bytes(value, plugin_id, sql),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

/// How many times a poisoned-connection 0x00 error is retried on a fresh pooled
/// connection before the guaranteed last-resort brand-new-connection fallback
/// below takes over. The desync is rare, so a pool retry usually clears it; this
/// cap just bounds the probabilistic retry.
const POISONED_CONNECTION_RETRIES: u32 = 6;

/// True when a DB error is the server-side SQLSTATE 22021 "invalid byte sequence
/// for encoding UTF8: 0x00". Because every value we bind is sanitized + swept
/// NUL-free at the choke point, this error does NOT mean our data is dirty - it
/// means the pooled sqlx connection is DESYNCED: sqlx 0.8.6 has no `Drop` reset
/// for `PgConnection`, so a query future dropped mid-flush (our cross-thread
/// `block_on(execute_raw)` bridge can produce this under load) leaves a partial
/// message in the persistent write buffer, and `ping()` on pool checkout
/// flushes that dirty tail (length-prefix/NUL framing bytes, which contain 0x00)
/// as the prefix of the NEXT statement. The cure is to retry on a different
/// connection: `execute_raw` pool-acquires a fresh one each call, and the 0x00
/// is rejected by Postgres before execution, so retrying is side-effect-free.
fn is_poisoned_connection_nul_error(e: &sea_orm::DbErr) -> bool {
    let msg = e.to_string();
    msg.contains("0x00") && msg.contains("invalid byte sequence")
}

fn json_value_contains_nul(v: &JsonValue) -> bool {
    match v {
        JsonValue::String(s) => s.contains('\0'),
        JsonValue::Array(a) => a.iter().any(json_value_contains_nul),
        JsonValue::Object(o) => o
            .iter()
            .any(|(k, val)| k.contains('\0') || json_value_contains_nul(val)),
        _ => false,
    }
}

/// Final, unbypassable NUL strip on the fully-built bound values, applied at the
/// execute choke point right before `Statement::from_sql_and_values`. PostgreSQL
/// TEXT columns reject 0x00; this guarantees no NUL reaches libpq regardless of
/// what any (possibly stale or future) plugin/SDK sent, independent of the
/// earlier per-value sanitizers in `json_to_sea_value`. Returns how many values
/// were modified so callers can surface that a NUL slipped past upstream layers.
fn strip_nul_from_sea_values(values: &mut [sea_orm::Value], plugin_id: &str, sql: &str) -> usize {
    let mut stripped = 0usize;
    for value in values.iter_mut() {
        match value {
            sea_orm::Value::String(Some(s)) if s.contains('\0') => {
                let cleaned = s.replace('\0', "\u{FFFD}");
                *s = cleaned;
                stripped += 1;
            }
            sea_orm::Value::Json(Some(j)) if json_value_contains_nul(j) => {
                let cleaned = sanitize_json_nul_bytes((**j).clone(), plugin_id, sql);
                **j = cleaned;
                stripped += 1;
            }
            _ => {}
        }
    }
    stripped
}

fn parse_args(
    args_json: &str,
    plugin_id: &str,
    sql: &str,
) -> Result<Vec<sea_orm::Value>, extism::Error> {
    if args_json.trim().is_empty() {
        return Ok(vec![]);
    }
    let json_arr: Vec<JsonValue> = serde_json::from_str(args_json)
        .map_err(|e| extism::Error::msg(format!("Invalid args JSON: {}", e)))?;

    Ok(json_arr
        .into_iter()
        .map(|v| json_to_sea_value(v, plugin_id, sql))
        .collect())
}

host_fn!(pub db_execute(user_data: (String, DatabaseConnection); sql: String, args: String) -> String {
    // Clone the connection handle out of the UserData lock and release the lock
    // BEFORE the blocking DB call. `DatabaseConnection` is a cheap pool handle;
    // holding the std `Mutex` across `block_on(execute_raw)` serializes every
    // plugin DB call on one mutex AND pins the lock across an await point - an
    // anti-pattern. `db_begin` already clones-then-releases; match it here.
    let (plugin_id, db) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.1.clone())
    };
    let span = super::host_fn_span("db_execute", &plugin_id);
    let _enter = span.enter();

    raw_sql_gate(raw_plugin_sql_disabled(), &plugin_id)?;
    let sql = sanitize_sql_text("sql", &plugin_id, sql);
    reject_role_escalation(&plugin_id, &sql)?;
    let args = sanitize_sql_text("args", &plugin_id, args);
    let mut values = parse_args(&args, &plugin_id, &sql)?;
    let stripped = strip_nul_from_sea_values(&mut values, &plugin_id, &sql);
    if stripped > 0 {
        tracing::warn!(
            plugin_id = %plugin_id,
            stripped,
            sql = %sql.chars().take(120).collect::<String>(),
            "Stripped NUL bytes from bound values at db_execute choke point"
        );
    }
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, &sql, values);

    let mut exec_result = tokio::runtime::Handle::current().block_on(async {
        db.execute_raw(stmt.clone()).await
    });
    let mut poison_retries = 0;
    while poison_retries < POISONED_CONNECTION_RETRIES
        && matches!(&exec_result, Err(e) if is_poisoned_connection_nul_error(e))
    {
        poison_retries += 1;
        tracing::warn!(
            plugin_id = %plugin_id,
            attempt = poison_retries,
            "db_execute hit a poisoned-connection 0x00 error on clean params; retrying on a fresh connection"
        );
        exec_result = tokio::runtime::Handle::current().block_on(async {
            db.execute_raw(stmt.clone()).await
        });
    }

    // GUARANTEED last resort: if every pool retry was exhausted by a
    // poisoned-connection 0x00, run the statement once on a brand-new
    // connection, which cannot carry stale wire framing. Turns the probabilistic
    // retry into a deterministic recovery.
    if matches!(&exec_result, Err(e) if is_poisoned_connection_nul_error(e)) {
        tracing::warn!(
            plugin_id = %plugin_id,
            "db_execute exhausted pool retries on poisoned-connection 0x00; falling back to a fresh connection"
        );
        exec_result = execute_on_fresh_connection(stmt, FallbackRole::Restricted);
    }

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            let sql_preview: String = sql.chars().take(800).collect();
            let args_preview: String = args.chars().take(800).collect();
            error!(plugin_id = %plugin_id, sql_len = sql.len(), args_len = args.len(), poison_retries, sql = %sql_preview, args = %args_preview, "DB execution error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_query(user_data: (String, DatabaseConnection); sql: String, args: String) -> String {
    let (plugin_id, db) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.1.clone())
    };
    let span = super::host_fn_span("db_query", &plugin_id);
    let _enter = span.enter();

    raw_sql_gate(raw_plugin_sql_disabled(), &plugin_id)?;
    let sql = sanitize_sql_text("sql", &plugin_id, sql);
    reject_role_escalation(&plugin_id, &sql)?;
    let args = sanitize_sql_text("args", &plugin_id, args);
    let values = parse_args(&args, &plugin_id, &sql)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, wrapped_sql, values);

    let mut query_result = tokio::runtime::Handle::current().block_on(async {
        db.query_one_raw(stmt.clone()).await
    });
    let mut poison_retries = 0;
    while poison_retries < POISONED_CONNECTION_RETRIES
        && matches!(&query_result, Err(e) if is_poisoned_connection_nul_error(e))
    {
        poison_retries += 1;
        tracing::warn!(
            plugin_id = %plugin_id,
            attempt = poison_retries,
            "db_query hit a poisoned-connection 0x00 error on clean params; retrying on a fresh connection"
        );
        query_result = tokio::runtime::Handle::current().block_on(async {
            db.query_one_raw(stmt.clone()).await
        });
    }

    // GUARANTEED last resort, symmetric with db_execute: if every pool retry was
    // exhausted by a poisoned-connection 0x00, run the query once on a brand-new
    // connection, which cannot carry stale wire framing. Without this a poisoned
    // db_query surfaces an error to the plugin, which a plugin turns into a
    // SystemError - a system condition standing as the contestant's verdict.
    if matches!(&query_result, Err(e) if is_poisoned_connection_nul_error(e)) {
        tracing::warn!(
            plugin_id = %plugin_id,
            "db_query exhausted pool retries on poisoned-connection 0x00; falling back to a fresh connection"
        );
        query_result = query_on_fresh_connection(stmt, FallbackRole::Restricted);
    }

    match query_result {
        Ok(Some(res)) => {
            let json_val: serde_json::Value = res.try_get("", "json_data")
                .unwrap_or(serde_json::json!([]));
            HostDbResponse::ok(json_val).to_json_string()
        },
        Ok(None) => HostDbResponse::ok(serde_json::json!([])).to_json_string(),
        Err(e) => {
            error!("DB query error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_begin(user_data: (String, DatabaseConnection, TransactionMap); _input: String) -> String {
    let (plugin_id, db, txn_map) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.1.clone(), ctx.2.clone())
    };
    let span = super::host_fn_span("db_begin", &plugin_id);
    let _enter = span.enter();

    raw_sql_gate(raw_plugin_sql_disabled(), &plugin_id)?;

    // Per-plugin cap on concurrently open transactions, checked BEFORE
    // checking out a pool connection: each open transaction pins one plugin
    // DB pool connection, so without a cap a plugin that keeps opening
    // transactions (or trapping after db_begin) exhausts the pool for everyone.
    {
        let map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        let open = map_guard.values().filter(|t| t.plugin_id == plugin_id).count();
        if open >= MAX_OPEN_TXNS_PER_PLUGIN {
            tracing::warn!(
                plugin_id = %plugin_id,
                open,
                cap = MAX_OPEN_TXNS_PER_PLUGIN,
                "db_begin rejected: plugin hit its open-transaction cap"
            );
            return HostDbResponse::err(format!(
                "Too many open transactions for this plugin (max {MAX_OPEN_TXNS_PER_PLUGIN}); commit or roll back existing transactions first"
            )).to_json_string();
        }
    }

    let txn = tokio::runtime::Handle::current()
        .block_on(db.begin())
        .map_err(|e| {
            error!("DB begin error: {}", e);
            extism::Error::msg(e.to_string())
        })?;

    let txn_id = Uuid::new_v4().to_string();
    txn_map.lock()
        .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?
        .insert(txn_id.clone(), PluginTransaction {
            txn,
            plugin_id: plugin_id.clone(),
            last_used: Instant::now(),
        });
    // Guarantees cleanup if the guest traps before commit/rollback.
    spawn_txn_reaper(txn_map.clone(), txn_id.clone());

    HostDbResponse::ok(serde_json::json!({"txn_id": txn_id})).to_json_string()
});

host_fn!(pub db_query_in(user_data: (String, DatabaseConnection, TransactionMap); txn_id: String, sql: String, args: String) -> String {
    let (plugin_id, txn_map) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.2.clone())
    };
    let span = super::host_fn_span("db_query_in", &plugin_id);
    let _enter = span.enter();

    raw_sql_gate(raw_plugin_sql_disabled(), &plugin_id)?;
    let sql = sanitize_sql_text("sql", &plugin_id, sql);
    reject_role_escalation(&plugin_id, &sql)?;
    let args = sanitize_sql_text("args", &plugin_id, args);
    let values = parse_args(&args, &plugin_id, &sql)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, wrapped_sql, values);

    let mut map_guard = txn_map.lock()
        .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
    let entry = lookup_owned_txn(&mut map_guard, &txn_id, &plugin_id, "db_query_in")?;
    entry.last_used = Instant::now();

    let query_result = tokio::runtime::Handle::current().block_on(entry.txn.query_one_raw(stmt));
    entry.last_used = Instant::now();

    match query_result {
        Ok(Some(res)) => {
            let json_val: serde_json::Value = res.try_get("", "json_data")
                .unwrap_or(serde_json::json!([]));
            HostDbResponse::ok(json_val).to_json_string()
        },
        Ok(None) => HostDbResponse::ok(serde_json::json!([])).to_json_string(),
        Err(e) => {
            error!("DB query_in error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_execute_in(user_data: (String, DatabaseConnection, TransactionMap); txn_id: String, sql: String, args: String) -> String {
    let (plugin_id, txn_map) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.2.clone())
    };
    let span = super::host_fn_span("db_execute_in", &plugin_id);
    let _enter = span.enter();

    raw_sql_gate(raw_plugin_sql_disabled(), &plugin_id)?;
    let sql = sanitize_sql_text("sql", &plugin_id, sql);
    reject_role_escalation(&plugin_id, &sql)?;
    let args = sanitize_sql_text("args", &plugin_id, args);
    let mut values = parse_args(&args, &plugin_id, &sql)?;
    let stripped = strip_nul_from_sea_values(&mut values, &plugin_id, &sql);
    if stripped > 0 {
        tracing::warn!(
            plugin_id = %plugin_id,
            stripped,
            sql = %sql.chars().take(120).collect::<String>(),
            "Stripped NUL bytes from bound values at db_execute_in choke point"
        );
    }
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, sql, values);

    let mut map_guard = txn_map.lock()
        .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
    let entry = lookup_owned_txn(&mut map_guard, &txn_id, &plugin_id, "db_execute_in")?;
    entry.last_used = Instant::now();

    let exec_result = tokio::runtime::Handle::current().block_on(entry.txn.execute_raw(stmt));
    entry.last_used = Instant::now();

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            error!("DB execute_in error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_commit(user_data: (String, DatabaseConnection, TransactionMap); txn_id: String) -> String {
    let (plugin_id, txn_map) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.2.clone())
    };
    let span = super::host_fn_span("db_commit", &plugin_id);
    let _enter = span.enter();

    let entry = {
        let mut map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        // Ownership check first; only the opening plugin may commit.
        lookup_owned_txn(&mut map_guard, &txn_id, &plugin_id, "db_commit")?;
        map_guard.remove(&txn_id)
            .ok_or_else(|| extism::Error::msg(format!("Transaction not found: {txn_id}")))?
    };

    let result = tokio::runtime::Handle::current().block_on(entry.txn.commit());

    match result {
        Ok(()) => HostDbResponse::ok(serde_json::json!({"ok": true})).to_json_string(),
        Err(e) => {
            error!("DB commit error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_rollback(user_data: (String, DatabaseConnection, TransactionMap); txn_id: String) -> String {
    let (plugin_id, txn_map) = {
        let user_data_guard = user_data.get()?;
        let ctx = super::lock_or_poison(&user_data_guard)?;
        (ctx.0.clone(), ctx.2.clone())
    };
    let span = super::host_fn_span("db_rollback", &plugin_id);
    let _enter = span.enter();

    let entry = {
        let mut map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        match map_guard.get(&txn_id) {
            // A foreign transaction must behave exactly like a missing one
            // (rollback of a missing txn is an idempotent no-op success), so
            // one plugin can neither roll back nor probe another's txn.
            Some(entry) if entry.plugin_id != plugin_id => {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    owner = %entry.plugin_id,
                    txn_id = %txn_id,
                    "db_rollback: plugin attempted to roll back a DB transaction it does not own"
                );
                None
            }
            Some(_) => map_guard.remove(&txn_id),
            None => None,
        }
    };

    match entry {
        Some(entry) => {
            let result = tokio::runtime::Handle::current().block_on(entry.txn.rollback());
            match result {
                Ok(()) => HostDbResponse::ok(serde_json::json!({"ok": true})).to_json_string(),
                Err(e) => {
                    error!("DB rollback error: {}", e);
                    HostDbResponse::err(e.to_string()).to_json_string()
                }
            }
        }
        None => HostDbResponse::ok(serde_json::json!({"ok": true})).to_json_string(),
    }
});

/// Run a plugin-authored write on the shared plugin DB pool and return the same
/// `HostDbResponse` JSON envelope `db_execute` produces (`{"data": <rows>}` or
/// `{"error": <msg>}`). Shared by the capability-gated `host.submission.*` write
/// host functions, which build the SQL server-side but must hit the DB through
/// the exact same NUL-sanitization, arg-parsing, and poisoned-connection
/// recovery path as raw `db_execute`. Kept as a separate helper (rather than
/// refactoring `db_execute` to call it) so the raw-SQL machinery above stays
/// byte-for-byte unchanged in this phase.
pub(super) fn execute_on_pool(
    db: &DatabaseConnection,
    plugin_id: &str,
    sql: String,
    args: String,
) -> Result<String, extism::Error> {
    let sql = sanitize_sql_text("sql", plugin_id, sql);
    let args = sanitize_sql_text("args", plugin_id, args);
    let mut values = parse_args(&args, plugin_id, &sql)?;
    let stripped = strip_nul_from_sea_values(&mut values, plugin_id, &sql);
    if stripped > 0 {
        tracing::warn!(
            plugin_id = %plugin_id,
            stripped,
            sql = %sql.chars().take(120).collect::<String>(),
            "Stripped NUL bytes from bound values at submission execute choke point"
        );
    }
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, &sql, values);

    let mut exec_result =
        tokio::runtime::Handle::current().block_on(async { db.execute_raw(stmt.clone()).await });
    let mut poison_retries = 0;
    while poison_retries < POISONED_CONNECTION_RETRIES
        && matches!(&exec_result, Err(e) if is_poisoned_connection_nul_error(e))
    {
        poison_retries += 1;
        tracing::warn!(
            plugin_id = %plugin_id,
            attempt = poison_retries,
            "submission execute hit a poisoned-connection 0x00 error on clean params; retrying on a fresh connection"
        );
        exec_result = tokio::runtime::Handle::current()
            .block_on(async { db.execute_raw(stmt.clone()).await });
    }

    if matches!(&exec_result, Err(e) if is_poisoned_connection_nul_error(e)) {
        tracing::warn!(
            plugin_id = %plugin_id,
            "submission execute exhausted pool retries on poisoned-connection 0x00; falling back to a fresh connection"
        );
        exec_result = execute_on_fresh_connection(stmt, FallbackRole::Privileged);
    }

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            let sql_preview: String = sql.chars().take(800).collect();
            let args_preview: String = args.chars().take(800).collect();
            error!(plugin_id = %plugin_id, sql_len = sql.len(), args_len = args.len(), poison_retries, sql = %sql_preview, args = %args_preview, "Submission DB execution error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
}

/// Read-side counterpart to [`execute_on_pool`]: run a plugin-authored read on
/// the shared plugin DB pool, json_agg-wrapped exactly like `db_query`, and
/// return the same `HostDbResponse` JSON envelope (`{"data": [rows]}`). Used by
/// `host.submission.query_test_cases`.
pub(super) fn query_on_pool(
    db: &DatabaseConnection,
    plugin_id: &str,
    sql: String,
    args: String,
) -> Result<String, extism::Error> {
    let sql = sanitize_sql_text("sql", plugin_id, sql);
    let args = sanitize_sql_text("args", plugin_id, args);
    let values = parse_args(&args, plugin_id, &sql)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = Statement::from_sql_and_values(DbBackend::Postgres, wrapped_sql, values);

    let mut query_result =
        tokio::runtime::Handle::current().block_on(async { db.query_one_raw(stmt.clone()).await });
    let mut poison_retries = 0;
    while poison_retries < POISONED_CONNECTION_RETRIES
        && matches!(&query_result, Err(e) if is_poisoned_connection_nul_error(e))
    {
        poison_retries += 1;
        tracing::warn!(
            plugin_id = %plugin_id,
            attempt = poison_retries,
            "submission query hit a poisoned-connection 0x00 error on clean params; retrying on a fresh connection"
        );
        query_result = tokio::runtime::Handle::current()
            .block_on(async { db.query_one_raw(stmt.clone()).await });
    }

    if matches!(&query_result, Err(e) if is_poisoned_connection_nul_error(e)) {
        tracing::warn!(
            plugin_id = %plugin_id,
            "submission query exhausted pool retries on poisoned-connection 0x00; falling back to a fresh connection"
        );
        query_result = query_on_fresh_connection(stmt, FallbackRole::Privileged);
    }

    match query_result {
        Ok(Some(res)) => {
            let json_val: serde_json::Value = res
                .try_get("", "json_data")
                .unwrap_or(serde_json::json!([]));
            HostDbResponse::ok(json_val).to_json_string()
        }
        Ok(None) => HostDbResponse::ok(serde_json::json!([])).to_json_string(),
        Err(e) => {
            error!("Submission DB query error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectOptions, Database};

    use super::*;

    #[test]
    fn role_escalation_guard_blocks_every_escape_vector() {
        for sql in [
            "RESET ROLE",
            "reset role",
            "SET ROLE app_role",
            "set role \"app\"",
            "SET  ROLE app",
            // scoped variants: the optional SESSION/LOCAL keyword must not slip
            // a role change past a naive adjacent-pair match
            "SET LOCAL ROLE app_role",
            "SET SESSION ROLE app_role",
            "SET LOCAL role = app_role",
            "SET ROLE TO app_role",
            "RESET SESSION AUTHORIZATION",
            "RESET ALL",
            "DISCARD ALL",
            "SET SESSION AUTHORIZATION app_role",
            "SET SESSION_AUTHORIZATION app",
            "SELECT set_config('role', 'app_role', false)",
            // whitespace / comment before the paren must not defeat the guard
            "SELECT set_config ('role', 'none', false)",
            "SELECT pg_catalog.set_config\t('role','x',false)",
            "SELECT set_config/**/('role','x',false)",
            "/* sneaky */ RESET ROLE",
            "-- c\nRESET ROLE",
            "DO $$ BEGIN EXECUTE 'reset role'; END $$",
            "CREATE FUNCTION f() RETURNS void AS $$ RESET ROLE $$ LANGUAGE sql",
            // A role change smuggled in a SINGLE-QUOTED routine body is invisible
            // to the lexer (the body is blanked, unlike a verbatim `$$..$$`
            // body), so the CREATE FUNCTION/PROCEDURE construct is blocked
            // outright -- the named analogue of the DO block. Covers plpgsql/sql,
            // OR REPLACE, PROCEDURE, and a schema-qualified routine name.
            "CREATE FUNCTION evil() RETURNS void AS 'BEGIN PERFORM set_config(''role'',''postgres'',false); END' LANGUAGE plpgsql",
            "CREATE OR REPLACE FUNCTION evil() RETURNS void AS 'SELECT set_config(''role'',''postgres'',false)' LANGUAGE sql",
            "CREATE PROCEDURE evil() LANGUAGE plpgsql AS 'BEGIN PERFORM set_config(''role'',''x'',false); END'",
            "create function pg_temp.evil() returns void as 'select 1' language sql",
            // a SET ROLE smuggled after a benign statement must NOT hide behind
            // the earlier UPDATE: each ';'-separated statement is judged alone
            "UPDATE my_plugin_table SET v = 1 WHERE k = 2; SET ROLE app_role",
            "SELECT 1; RESET ROLE",
            "SELECT 1; DISCARD ALL",
            // a `/*` opened INSIDE a string literal is not a comment: a naive
            // stripper would enter comment mode and swallow the trailing
            // set_config, a fail-OPEN escalation. The lexer keeps it in the
            // string and still sees set_config in top-level text.
            "SELECT '/*', set_config('role', 'none', false)",
            "SELECT '/*' AS a, set_config('role','none',false)",
            // a quote inside a dollar-quoted literal must not open a real string
            // that swallows the following set_config
            "SELECT $$'$$, set_config('role','x',false)",
            // scoped SESSION AUTHORIZATION: the command word SESSION collides
            // with the scope word, so a scope-skip that consumes only the first
            // SESSION must still block these
            "SET LOCAL SESSION AUTHORIZATION app",
            "SET SESSION SESSION AUTHORIZATION app",
            // lexically unbalanced input fails CLOSED (treated as blocked)
            "SELECT 'abc",
            "SELECT $$abc",
            "SELECT /* abc",
            "SELECT \"abc",
            // the built-in set_config called via a QUOTED identifier is a real
            // role escape -- dropping the quoted body used to hide it
            "SELECT \"set_config\"('role','none',false)",
            "SELECT pg_catalog.\"set_config\"('role','none',false)",
            "SELECT \"pg_catalog\".\"set_config\"('role','none',false)",
            "SELECT COALESCE(json_agg(t), '[]') FROM (SELECT \"set_config\"('role','none',false)) AS t",
            "SET \"role\" = 'admin'",
            "SET \"session_authorization\" = 'admin'",
            // a standard string abutting a token that ends in `e` (name'..') is
            // NOT an E-string; the old heuristic read past its close and swallowed
            // the ';'-smuggled SET ROLE
            "SELECT name'abc\\'; SET ROLE admin; --'",
            // U&"..." Unicode-escape identifiers decode to the real name at parse
            // time, so flattening the literal escape text (`_0073et_config`) would
            // miss the keyword. `\0073`=s -> set_config; `\0072`=r -> role. Both
            // execute a role escape as a single statement, so both fail CLOSED.
            "SELECT U&\"\\0073et_config\"('role','none',false)",
            "SET U&\"\\0072ole\" = 'postgres'",
            "SET u&\"\\0072ole\" TO 'postgres'",
        ] {
            assert!(
                reject_role_escalation("p", sql).is_err(),
                "should have blocked: {sql}"
            );
        }
    }

    #[test]
    fn role_escalation_guard_allows_normal_plugin_sql() {
        for sql in [
            "SELECT * FROM submission WHERE id = $1",
            "INSERT INTO my_plugin_table (k, v) VALUES ($1, $2)",
            "UPDATE my_plugin_table SET v = $1 WHERE k = $2",
            "CREATE TABLE my_plugin_table (k TEXT PRIMARY KEY, v INT)",
            "ALTER TABLE my_plugin_table SET (fillfactor = 90)",
            // a table whose NAME is the reserved word `function` (quoted, so it
            // flattens to a `function` token) is not a routine definition: the
            // command keyword after CREATE is TABLE, so it stays allowed
            "CREATE TABLE \"function\" (x int)",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "DELETE FROM my_plugin_table WHERE k = $1",
            // a column named role*/session* must NOT be a false positive
            "UPDATE t SET role_id = $1 WHERE k = $2",
            // a column literally named `role`: `UPDATE ... SET role = ...` is a
            // DML assignment (preceded by UPDATE), not the SET ROLE command, so a
            // plugin with full DML on its own table must be allowed
            "UPDATE my_plugin_table SET role = $1 WHERE k = $2",
            "INSERT INTO acl (k, role) VALUES ($1, $2) ON CONFLICT (k) DO UPDATE SET role = $2",
            "INSERT INTO sessions (session_token) VALUES ($1)",
            "SELECT * FROM roles WHERE name = $1",
            "SET statement_timeout = 5000",
            // scope keyword before a NON-role parameter must stay allowed
            "SET LOCAL statement_timeout = 5000",
            "SET SESSION search_path TO my_schema",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            // role keywords or ';' that appear INSIDE a string literal are data,
            // not commands: the lexer neutralizes the literal so they must not
            // trip the guard (previously false-positive-rejected)
            "INSERT INTO t(v) VALUES ('hello; discard all')",
            "UPDATE t SET note = 'apply; set role reviewer' WHERE id = 1",
            "INSERT INTO audit_log(action) VALUES ('reset all')",
            "CREATE TABLE t (status text DEFAULT 'set role')",
            // '' and E'\' escapes inside a string keep the lexer in the string
            "SELECT 'O''Brien' AS name",
            "SELECT E'line\\n' AS x",
            // a quoted identifier that merely looks like a command is a column:
            // it flattens to ONE token, never the SET ROLE keyword pair
            "SELECT \"set role\" FROM weird_table",
            "SELECT \"role\", x FROM t",
            "UPDATE t SET \"role\" = $1 WHERE id = $2",
            "INSERT INTO t (\"role\") VALUES ($1)",
            // dollar-quoted DATA with no role keyword stays allowed
            "INSERT INTO t(doc) VALUES ($$ hello ; world $$)",
            // a standard string after an e-ending token, ending in a backslash,
            // is valid and must not be mis-read as an E-string (was Err before)
            "SELECT name'C:\\'",
            "SELECT p FROM t WHERE p LIKE'abc\\'",
            // a genuine standalone E-string still honors its backslash escapes
            "SELECT E'a\\'b' AS x",
            // the U&" fail-closed must NOT over-block a plain `&` (bitwise AND)
            // that merely abuts a normal quoted identifier: the `U`/`u` has to be
            // its OWN token for Postgres to read a Unicode-escape identifier
            "SELECT id & \"role\" FROM t",
            "SELECT nu&\"x\" FROM t",
        ] {
            assert!(
                reject_role_escalation("p", sql).is_ok(),
                "should have allowed: {sql}"
            );
        }
    }

    #[test]
    fn raw_sql_gate_fails_closed_only_when_disabled() {
        // Enabled (the normal path: a dedicated login or an operator plugin_url):
        // raw SQL is allowed through to the per-statement guard.
        assert!(raw_sql_gate(false, "p").is_ok());
        // Degraded to app-role auth: raw SQL is refused outright, with a message
        // that points the operator at the two supported remedies.
        let err = raw_sql_gate(true, "p").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("host.db.*") && msg.contains("disabled"),
            "{msg}"
        );
        assert!(
            msg.contains("CREATEROLE") && msg.contains("plugin_url"),
            "{msg}"
        );
    }

    #[test]
    fn detects_poisoned_connection_nul_error() {
        let poisoned = sea_orm::DbErr::Custom(
            "error returned from database: invalid byte sequence for encoding \"UTF8\": 0x00"
                .to_string(),
        );
        assert!(is_poisoned_connection_nul_error(&poisoned));

        let unrelated = sea_orm::DbErr::Custom("duplicate key value violates unique".to_string());
        assert!(!is_poisoned_connection_nul_error(&unrelated));
    }

    #[test]
    fn strip_nul_from_sea_values_cleans_string_and_json_and_counts() {
        let mut values = vec![
            sea_orm::Value::String(Some("ok\0bad".to_string())),
            sea_orm::Value::BigInt(Some(42)),
            sea_orm::Value::Json(Some(Box::new(serde_json::json!({"k": "a\u{0000}b"})))),
            sea_orm::Value::String(Some("clean".to_string())),
            sea_orm::Value::String(None),
        ];

        let stripped = strip_nul_from_sea_values(&mut values, "test-plugin", "INSERT ...");
        assert_eq!(stripped, 2, "two values carried NUL");

        match &values[0] {
            sea_orm::Value::String(Some(s)) => {
                assert_eq!(s.as_str(), "ok\u{FFFD}bad");
                assert!(!s.contains('\0'));
            }
            other => panic!("unexpected: {other:?}"),
        }
        match &values[2] {
            sea_orm::Value::Json(Some(j)) => {
                assert_eq!(j["k"], "a\u{FFFD}b");
                assert!(!json_value_contains_nul(j));
            }
            other => panic!("unexpected: {other:?}"),
        }
        // Untouched value is unchanged.
        match &values[3] {
            sea_orm::Value::String(Some(s)) => assert_eq!(s.as_str(), "clean"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn strip_nul_no_op_when_clean() {
        let mut values = vec![
            sea_orm::Value::String(Some("hello".to_string())),
            sea_orm::Value::Json(Some(Box::new(serde_json::json!(["a", "b"])))),
        ];
        assert_eq!(strip_nul_from_sea_values(&mut values, "p", "sql"), 0);
    }

    #[test]
    fn parse_args_sanitizes_top_level_nul_string() {
        let values = parse_args(r#"["ok\u0000bad"]"#, "test-plugin", "select $1").unwrap();

        match &values[0] {
            sea_orm::Value::String(Some(value)) => {
                assert_eq!(value, "ok\u{FFFD}bad");
                assert!(!value.contains('\0'));
            }
            other => panic!("unexpected value: {other:?}"),
        }
    }

    /// The read-side fresh-connection fallback must actually open a brand-new
    /// connection, run the json_agg-wrapped query, and return the rows in the
    /// same shape `db_query` produces. This is the deterministic recovery that
    /// keeps a poisoned `db_query` from surfacing a `SystemError` to the plugin.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_on_fresh_connection_runs_query_on_a_new_connection() {
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

        // Mirrors the json_agg wrapping `db_query` applies, so this exercises the
        // exact result shape the fallback must return.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM (SELECT 7 AS n) AS t",
            Vec::<sea_orm::Value>::new(),
        );

        // The helper uses `Handle::current().block_on`, exactly as the `db_query`
        // host function does in production, where it runs on a `spawn_blocking`
        // thread (not a runtime worker). Replicate that here so `block_on` is
        // legal - calling it directly on the test's runtime thread would panic.
        let row = tokio::task::spawn_blocking(move || {
            query_on_fresh_connection_url(&url, stmt, FallbackRole::Privileged, None)
        })
        .await
        .expect("blocking task joins")
        .expect("fresh-connection query should succeed")
        .expect("a row is returned");
        let json: serde_json::Value = row
            .try_get("", "json_data")
            .expect("json_data column present");
        assert_eq!(json, serde_json::json!([{ "n": 7 }]));
    }

    /// Security regression: the raw `sql` path opens its 0x00 fallback with
    /// [`FallbackRole::Restricted`], which must run AS `broccoli_plugin`, not the
    /// app role - the fallback must not become a privilege-escalation hatch. To
    /// probe that, this test gives a stand-in `broccoli_plugin` role SELECT but no
    /// INSERT on a table (a deliberately minimal grant, NOT the production migrated
    /// role, which now has full DML) and asserts the fallback's INSERT is denied:
    /// that can only happen if the fallback downshifted rather than staying app.
    /// The structured path's [`FallbackRole::Privileged`] keeps the app role so
    /// its server-owned core writes still land.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_fallback_denies_writes_the_plugin_role_lacks() {
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

        // A deliberately minimal stand-in role: NOLOGIN, app-role is a member,
        // SELECT-only on the table under test (no INSERT). This is NOT the
        // production role (which has full DML); the missing INSERT is the probe
        // that proves the fallback runs AS this role, not the app role.
        let admin = Database::connect(url.clone())
            .await
            .expect("connect as app role");
        for ddl in [
            "CREATE ROLE broccoli_plugin NOLOGIN",
            "GRANT broccoli_plugin TO CURRENT_USER",
            "CREATE TABLE core_t (id int)",
            "GRANT SELECT ON core_t TO broccoli_plugin",
        ] {
            admin
                .execute_raw(Statement::from_string(DbBackend::Postgres, ddl))
                .await
                .unwrap_or_else(|e| panic!("setup `{ddl}` failed: {e}"));
        }

        // Restricted fallback: the INSERT the role lacks must be denied.
        let insert = Statement::from_string(DbBackend::Postgres, "INSERT INTO core_t VALUES (1)");
        let denied_url = url.clone();
        let denied = tokio::task::spawn_blocking(move || {
            execute_on_fresh_connection_url(&denied_url, insert, FallbackRole::Restricted, None)
        })
        .await
        .expect("blocking task joins");
        assert!(
            denied.is_err(),
            "a restricted fallback must not perform a write the plugin role lacks"
        );

        // Restricted fallback read: SELECT is granted, so it must still succeed.
        let read = Statement::from_string(
            DbBackend::Postgres,
            "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM (SELECT id FROM core_t) AS t",
        );
        let read_url = url.clone();
        let read_ok = tokio::task::spawn_blocking(move || {
            query_on_fresh_connection_url(&read_url, read, FallbackRole::Restricted, None)
        })
        .await
        .expect("blocking task joins");
        assert!(
            read_ok.is_ok(),
            "a restricted fallback must still perform a granted read"
        );

        // Privileged fallback: the same INSERT runs as the app role and lands.
        let privileged_insert =
            Statement::from_string(DbBackend::Postgres, "INSERT INTO core_t VALUES (2)");
        let priv_url = url.clone();
        let allowed = tokio::task::spawn_blocking(move || {
            execute_on_fresh_connection_url(
                &priv_url,
                privileged_insert,
                FallbackRole::Privileged,
                None,
            )
        })
        .await
        .expect("blocking task joins");
        assert!(
            allowed.is_ok(),
            "a privileged fallback keeps the app role so server-owned writes land"
        );
    }

    /// A transaction orphaned by a trapped guest (never committed or rolled
    /// back) must be rolled back by the reaper and its pooled connection
    /// returned. The pool is sized at ONE connection, so the second `begin`
    /// can only succeed if the reaper actually released the orphan's
    /// connection back to the pool.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn orphaned_transaction_is_reaped_and_returns_its_pool_connection() {
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

        let mut opt = ConnectOptions::new(url);
        opt.max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let db = Database::connect(opt).await.expect("connect");

        let txn = db.begin().await.expect("begin orphan txn");
        let txn_map: TransactionMap = Arc::new(StdMutex::new(HashMap::new()));
        let txn_id = "orphan-txn".to_string();
        txn_map.lock().unwrap().insert(
            txn_id.clone(),
            PluginTransaction {
                txn,
                plugin_id: "trapped-plugin".to_string(),
                last_used: Instant::now(),
            },
        );

        spawn_txn_reaper_with(
            txn_map.clone(),
            txn_id,
            Duration::ZERO,
            Duration::from_millis(50),
        );

        // The pool's only connection is pinned by the orphan until the reaper
        // rolls it back; this begin succeeding proves it was returned.
        let reclaimed = tokio::time::timeout(Duration::from_secs(20), db.begin())
            .await
            .expect("reaper should return the orphaned connection to the pool")
            .expect("begin after reap");
        reclaimed.rollback().await.expect("rollback");
        assert!(
            txn_map.lock().unwrap().is_empty(),
            "reaper should remove the orphaned entry from the map"
        );
    }

    /// A txn id owned by another plugin must be rejected exactly like a
    /// nonexistent one, while the owner keeps full access.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreign_transaction_id_is_rejected_like_missing() {
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
        let db = Database::connect(url).await.expect("connect");

        let txn = db.begin().await.expect("begin");
        let mut map: HashMap<String, PluginTransaction> = HashMap::new();
        map.insert(
            "txn-1".to_string(),
            PluginTransaction {
                txn,
                plugin_id: "owner".to_string(),
                last_used: Instant::now(),
            },
        );

        let foreign = lookup_owned_txn(&mut map, "txn-1", "intruder", "db_query_in")
            .expect_err("foreign txn id must be rejected");
        assert_eq!(
            foreign.to_string(),
            "Transaction not found: txn-1",
            "foreign txn must be indistinguishable from a missing one"
        );
        let missing = lookup_owned_txn(&mut map, "txn-2", "intruder", "db_query_in")
            .expect_err("missing txn id must be rejected");
        assert_eq!(missing.to_string(), "Transaction not found: txn-2");

        assert!(
            lookup_owned_txn(&mut map, "txn-1", "owner", "db_query_in").is_ok(),
            "owner keeps access to its own transaction"
        );
        assert!(
            map.contains_key("txn-1"),
            "rejected lookups must not remove the entry"
        );
    }

    #[test]
    fn parse_args_sanitizes_nested_json_nul_strings() {
        let values = parse_args(
            r#"[{"std\u0000out":"ok\u0000bad","nested":["x\u0000y"]}]"#,
            "test-plugin",
            "select $1",
        )
        .unwrap();

        match &values[0] {
            sea_orm::Value::Json(Some(value)) => {
                assert_eq!(value["std\u{FFFD}out"], "ok\u{FFFD}bad");
                assert_eq!(value["nested"][0], "x\u{FFFD}y");
                assert!(!value.to_string().contains('\0'));
            }
            other => panic!("unexpected value: {other:?}"),
        }
    }
}
