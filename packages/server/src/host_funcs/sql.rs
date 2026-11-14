use common::db::{DatabaseConnection, DatabaseTransaction};
use tokio::sync::OwnedSemaphorePermit;

use crate::plugin_db::PluginSqlPools;

/// What the raw `sql` host functions run with: the plugin pools, and the
/// plugin registry the declared read scopes (`[server.sql] read`) come from.
#[derive(Clone)]
pub struct PluginSql {
    pub pools: PluginSqlPools,
    pub registry: plugin_core::registry::PluginRegistry,
}

impl PluginSql {
    /// The content columns `plugin_id`'s loaded manifest declares.
    fn read_scopes(&self, plugin_id: &str) -> Vec<plugin_core::manifest::SqlReadScope> {
        self.registry
            .read()
            .ok()
            .and_then(|registry| {
                registry
                    .get(plugin_id)
                    .and_then(|entry| entry.manifest.server.as_ref())
                    .map(|server| server.sql.read.clone())
            })
            .unwrap_or_default()
    }

    async fn lease(
        &self,
        plugin_id: &str,
        call: Option<&plugin_core::executor::CallLocals>,
    ) -> Result<crate::plugin_db::Lease, crate::plugin_db::Unavailable> {
        let read = self.read_scopes(plugin_id);
        self.pools.lease(plugin_id, &read, call).await
    }
}
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, TryLockError};
use std::time::{Duration, Instant};

use extism::host_fn;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tracing::error;
use uuid::Uuid;

/// A plugin-opened DB transaction plus the bookkeeping needed to keep it from
/// leaking or being hijacked: the owning plugin (txn ids are only usable by the
/// plugin that opened them) and, behind the transaction's own lock, the
/// transaction itself and the last time the plugin touched it (drives the
/// idle-TTL reaper).
#[derive(Debug)]
pub struct PluginTransaction {
    plugin_id: String,
    state: StdMutex<TxnState>,
    /// The opening call's permit from the plugin SQL budget, held until the
    /// transaction ends and this entry is dropped.
    _permit: Arc<OwnedSemaphorePermit>,
}

#[derive(Debug)]
struct TxnState {
    /// `None` once commit, rollback or the reaper has taken the transaction;
    /// a statement that looked the entry up just before that sees it as gone.
    txn: Option<DatabaseTransaction>,
    last_used: Instant,
}

impl PluginTransaction {
    fn new(txn: DatabaseTransaction, plugin_id: String, permit: Arc<OwnedSemaphorePermit>) -> Self {
        Self {
            plugin_id,
            state: StdMutex::new(TxnState {
                txn: Some(txn),
                last_used: Instant::now(),
            }),
            _permit: permit,
        }
    }

    /// Take the transaction out for commit or rollback, waiting for any
    /// in-flight statement on it to finish first.
    fn take(&self) -> Option<DatabaseTransaction> {
        match self.state.lock() {
            Ok(mut state) => state.txn.take(),
            Err(poisoned) => poisoned.into_inner().txn.take(),
        }
    }
}

/// Every open plugin transaction, server-wide. The map lock is held only to
/// look an entry up, insert or remove it; statements run under the entry's own
/// lock, so plugins' transactions never wait on each other.
pub type TransactionMap = Arc<StdMutex<HashMap<String, Arc<PluginTransaction>>>>;

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
pub(crate) const MAX_OPEN_TXNS_PER_PLUGIN: usize = 8;

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
                // The map lock is only ever held for a lookup or an
                // insert/remove, never across a DB call. A poisoned map is
                // still a valid HashMap at every await-free point, and its
                // transactions still need reaping.
                let mut map = txn_map.lock().unwrap_or_else(|p| p.into_inner());
                // Committed or rolled back through the normal path.
                let Some(entry) = map.get(&txn_id).cloned() else {
                    return;
                };
                // try_lock: a held transaction lock means a statement is
                // running on it right now, so it is not idle. Skip the tick
                // instead of parking a runtime worker on a std mutex.
                let mut state = match entry.state.try_lock() {
                    Ok(state) => state,
                    Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                    Err(TryLockError::WouldBlock) => continue,
                };
                if state.last_used.elapsed() < idle_ttl {
                    None
                } else {
                    map.remove(&txn_id);
                    state.txn.take().map(|txn| (entry.plugin_id.clone(), txn))
                }
            };
            if let Some((plugin_id, txn)) = orphaned {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    txn_id = %txn_id,
                    idle_ttl_secs = idle_ttl.as_secs(),
                    "Rolling back orphaned plugin DB transaction (guest never committed or rolled back; likely trapped)"
                );
                if let Err(e) = txn.rollback().await {
                    error!(txn_id = %txn_id, "Failed to roll back orphaned plugin DB transaction: {}", e);
                }
                return;
            }
        }
    });
}

/// Fail the running call as unavailable: the plugin's SQL cannot run right
/// now (no connection free in time, or its role unprovisioned), which says
/// nothing about the request, so callers answer it with "try again".
fn unavailable(e: crate::plugin_db::Unavailable) -> extism::Error {
    tracing::warn!(error = %e, "Plugin SQL unavailable");
    plugin_core::executor::fail_unavailable();
    extism::Error::msg(e.to_string())
}

/// Look up a transaction by id, enforcing that the caller is the plugin that
/// opened it. Txn ids are unguessable UUIDs, but capability boundaries must
/// not rest on unguessability alone: a leaked id (logs, shared guest memory)
/// must not let one plugin commit, roll back, or query inside another
/// plugin's transaction. A foreign txn id is reported exactly like a
/// nonexistent one so plugins cannot probe for other plugins' transactions.
fn lookup_owned_txn<'a>(
    map: &'a HashMap<String, Arc<PluginTransaction>>,
    txn_id: &str,
    plugin_id: &str,
    host_fn: &str,
) -> Result<&'a Arc<PluginTransaction>, extism::Error> {
    match map.get(txn_id) {
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

/// Run `statement` inside the plugin's own transaction `txn_id`. The map lock
/// is held only for the ownership lookup; the statement runs under the
/// transaction's own lock, which serializes statements on that one
/// transaction (they share one connection) and nothing else.
fn with_owned_txn<R>(
    txn_map: &TransactionMap,
    txn_id: &str,
    plugin_id: &str,
    host_fn: &str,
    statement: impl FnOnce(&DatabaseTransaction) -> R,
) -> Result<R, extism::Error> {
    let entry = {
        let map = txn_map
            .lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        lookup_owned_txn(&map, txn_id, plugin_id, host_fn)?.clone()
    };
    let mut guard = entry
        .state
        .lock()
        .map_err(|_| extism::Error::msg("Transaction lock poisoned"))?;
    let state = &mut *guard;
    state.last_used = Instant::now();
    let txn = state
        .txn
        .as_ref()
        .ok_or_else(|| extism::Error::msg(format!("Transaction not found: {txn_id}")))?;
    let result = statement(txn);
    state.last_used = Instant::now();
    Ok(result)
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
            // escape. Keeping it as one token means the downstream set_config
            // match fires; dropping the body would hide it.
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
/// or authorization, runs an anonymous code block, or lifts a host-enforced
/// session bound ([`common::db::ENFORCED_SESSION_SETTINGS`]) for the rest of the
/// session. A session-level `SET statement_timeout = 0` would otherwise outlive
/// the call on the pooled connection and unbound every later plugin call that
/// reuses it; the transaction-scoped `SET LOCAL` form stays allowed. Role
/// changes are refused too. The engine already confines them: a plugin's pool
/// logs in as the plugin's own role (see [`crate::plugin_db`]), so `RESET ROLE`
/// lands on that role and `SET ROLE` reaches only its group role, which holds
/// less. The guard keeps a plugin from leaving its connection in a state its
/// next call does not expect, and fails closed on any occurrence of these
/// constructs anywhere in the comment-stripped statement. Applied only to the
/// raw plugin-authored entry points, never to the SQL host functions build.
fn reject_session_escape(plugin_id: &str, sql: &str) -> Result<(), extism::Error> {
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
            .any(statement_escapes_session),
        Err(()) => true,
    };
    if blocked {
        tracing::warn!(
            plugin_id,
            sql = %sql.chars().take(120).collect::<String>(),
            "Rejected a role/session change on the plugin SQL path",
        );
        return Err(extism::Error::msg(
            "not permitted on the plugin SQL capability: SET/RESET ROLE, SET SESSION AUTHORIZATION, set_config, DISCARD, anonymous code blocks, and session-level SET of host-enforced settings (statement_timeout, tcp_keepalives_*; use SET LOCAL) are forbidden",
        ));
    }
    Ok(())
}

/// True if a single comment-stripped, lowercased SQL statement changes the
/// session role/authorization, runs an anonymous code block, or defines a
/// routine (whose single-quoted body the lexer cannot see into). Callers pass
/// one ';'-separated statement (see [`reject_session_escape`]).
fn statement_escapes_session(stmt: &str) -> bool {
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
    // a distinct token) and `SET LOCAL statement_timeout = 5` untouched.
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
        // A session-scoped SET (plain or SESSION, not LOCAL) of a setting the
        // host enforces at connect time would persist on the pooled connection.
        let lifts_bound = t == "set"
            && after.first() != Some(&"local")
            && target.is_some_and(|t| common::db::ENFORCED_SESSION_SETTINGS.contains(&t));
        let changes = session_auth
            || lifts_bound
            || matches!(target, Some("role") | Some("authorization") | Some("all"));
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

fn json_to_sea_value(v: JsonValue) -> sea_orm::Value {
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
        JsonValue::String(s) => sea_orm::Value::String(Some(s)),
        v => sea_orm::Value::Json(Some(Box::new(v))),
    }
}

fn parse_args(args_json: &str) -> Result<Vec<sea_orm::Value>, extism::Error> {
    if args_json.trim().is_empty() {
        return Ok(vec![]);
    }
    let json_arr: Vec<JsonValue> = serde_json::from_str(args_json)
        .map_err(|e| extism::Error::msg(format!("Invalid args JSON: {}", e)))?;

    Ok(json_arr.into_iter().map(json_to_sea_value).collect())
}

/// The statement for plugin SQL bound with `values` from [`parse_args`].
///
/// sqlx keeps one prepared statement per SQL text on each connection, typed by
/// the parameters of its first execution, and later executions bind their
/// values in binary against those types. A plugin's JSON arguments change type
/// between calls of the same SQL (a number, then a string, then null), and an
/// int8 bound against a statement prepared for text reaches Postgres as text
/// bytes holding 0x00. The statement text therefore names its parameter types,
/// so each type signature prepares its own statement.
fn plugin_statement(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    let sql = if values.is_empty() {
        sql.to_string()
    } else {
        let types: Vec<String> = values
            .iter()
            .map(|value| format!("{:?}", value.array_type()))
            .collect();
        format!("/* {} */ {sql}", types.join(","))
    };
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

host_fn!(pub db_execute(user_data: Arc<(String, PluginSql)>; sql: String, args: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_execute")?;
    let (plugin_id, pools) = &*call;

    reject_session_escape(plugin_id, &sql)?;
    let values = parse_args(&args)?;
    let stmt = plugin_statement(&sql, values);

    let call = plugin_core::executor::call_locals();
    let exec_result = tokio::runtime::Handle::current().block_on(async {
        let lease = pools.lease(plugin_id, call.as_deref()).await;
        Ok(lease?.db.execute_raw(stmt).await)
    }).map_err(unavailable)?;

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            let sql_preview: String = sql.chars().take(800).collect();
            let args_preview: String = args.chars().take(800).collect();
            error!(plugin_id = %plugin_id, sql_len = sql.len(), args_len = args.len(), sql = %sql_preview, args = %args_preview, "DB execution error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_query(user_data: Arc<(String, PluginSql)>; sql: String, args: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_query")?;
    let (plugin_id, pools) = &*call;

    reject_session_escape(plugin_id, &sql)?;
    let values = parse_args(&args)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = plugin_statement(&wrapped_sql, values);

    let call = plugin_core::executor::call_locals();
    let query_result = tokio::runtime::Handle::current().block_on(async {
        let lease = pools.lease(plugin_id, call.as_deref()).await;
        Ok(lease?.db.query_one_raw(stmt).await)
    }).map_err(unavailable)?;

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

host_fn!(pub db_begin(user_data: Arc<(String, PluginSql, TransactionMap)>; _input: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_begin")?;
    let (plugin_id, pools, txn_map) = &*call;

    // Per-plugin cap on concurrently open transactions, checked BEFORE
    // checking out a pool connection: each open transaction pins one plugin
    // DB pool connection, so without a cap a plugin that keeps opening
    // transactions (or trapping after db_begin) exhausts the pool for everyone.
    {
        let map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        let open = map_guard.values().filter(|t| t.plugin_id == *plugin_id).count();
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

    let call = plugin_core::executor::call_locals();
    let lease = tokio::runtime::Handle::current()
        .block_on(pools.lease(plugin_id, call.as_deref()))
        .map_err(unavailable)?;
    let txn = tokio::runtime::Handle::current()
        .block_on(lease.db.begin())
        .map_err(|e| {
            error!("DB begin error: {}", e);
            extism::Error::msg(e.to_string())
        })?;
    let permit = lease.permit;

    let txn_id = Uuid::new_v4().to_string();
    txn_map.lock()
        .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?
        .insert(txn_id.clone(), Arc::new(PluginTransaction::new(txn, plugin_id.clone(), permit)));
    // Guarantees cleanup if the guest traps before commit/rollback.
    spawn_txn_reaper(txn_map.clone(), txn_id.clone());

    HostDbResponse::ok(serde_json::json!({"txn_id": txn_id})).to_json_string()
});

host_fn!(pub db_query_in(user_data: Arc<(String, PluginSql, TransactionMap)>; txn_id: String, sql: String, args: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_query_in")?;
    let (plugin_id, _, txn_map) = &*call;

    reject_session_escape(plugin_id, &sql)?;
    let values = parse_args(&args)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = plugin_statement(&wrapped_sql, values);

    let query_result = with_owned_txn(txn_map, &txn_id, plugin_id, "db_query_in", |txn| {
        tokio::runtime::Handle::current().block_on(txn.query_one_raw(stmt))
    })?;

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

host_fn!(pub db_execute_in(user_data: Arc<(String, PluginSql, TransactionMap)>; txn_id: String, sql: String, args: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_execute_in")?;
    let (plugin_id, _, txn_map) = &*call;

    reject_session_escape(plugin_id, &sql)?;
    let values = parse_args(&args)?;
    let stmt = plugin_statement(&sql, values);

    let exec_result = with_owned_txn(txn_map, &txn_id, plugin_id, "db_execute_in", |txn| {
        tokio::runtime::Handle::current().block_on(txn.execute_raw(stmt))
    })?;

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            error!("DB execute_in error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_commit(user_data: Arc<(String, PluginSql, TransactionMap)>; txn_id: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_commit")?;
    let (plugin_id, _, txn_map) = &*call;

    let entry = {
        let mut map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        // Ownership check first; only the opening plugin may commit.
        lookup_owned_txn(&map_guard, &txn_id, plugin_id, "db_commit")?;
        map_guard.remove(&txn_id)
            .ok_or_else(|| extism::Error::msg(format!("Transaction not found: {txn_id}")))?
    };
    let txn = entry.take()
        .ok_or_else(|| extism::Error::msg(format!("Transaction not found: {txn_id}")))?;

    let result = tokio::runtime::Handle::current().block_on(txn.commit());

    match result {
        Ok(()) => HostDbResponse::ok(serde_json::json!({"ok": true})).to_json_string(),
        Err(e) => {
            error!("DB commit error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
});

host_fn!(pub db_rollback(user_data: Arc<(String, PluginSql, TransactionMap)>; txn_id: String) -> String {
    let call = super::HostCall::enter(&user_data, "db_rollback")?;
    let (plugin_id, _, txn_map) = &*call;

    let entry = {
        let mut map_guard = txn_map.lock()
            .map_err(|_| extism::Error::msg("Transaction map lock poisoned"))?;
        match map_guard.get(&txn_id) {
            // A foreign transaction must behave exactly like a missing one
            // (rollback of a missing txn is an idempotent no-op success), so
            // one plugin can neither roll back nor probe another's txn.
            Some(entry) if entry.plugin_id != *plugin_id => {
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

    match entry.and_then(|entry| entry.take()) {
        Some(txn) => {
            let result = tokio::runtime::Handle::current().block_on(txn.rollback());
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

/// Run a statement a host function built on `db` (the host function pool) and
/// return the same `HostDbResponse` JSON envelope `db_execute` produces
/// (`{"data": <rows>}` or `{"error": <msg>}`). Shared by the capability-gated
/// `host.submission.*` and `host.code_run.*` writes, which bind their values
/// through the same argument parsing and statement typing as raw `db_execute`.
pub(super) fn execute_on_pool(
    db: &DatabaseConnection,
    plugin_id: &str,
    sql: String,
    args: String,
) -> Result<String, extism::Error> {
    let values = parse_args(&args)?;
    let stmt = plugin_statement(&sql, values);

    let exec_result = tokio::runtime::Handle::current().block_on(db.execute_raw(stmt));

    match exec_result {
        Ok(res) => HostDbResponse::ok(JsonValue::from(res.rows_affected())).to_json_string(),
        Err(e) => {
            let sql_preview: String = sql.chars().take(800).collect();
            let args_preview: String = args.chars().take(800).collect();
            error!(plugin_id = %plugin_id, sql_len = sql.len(), args_len = args.len(), sql = %sql_preview, args = %args_preview, "Submission DB execution error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
}

/// Read-side counterpart to [`execute_on_pool`]: run a read a host function
/// built on the host function pool, json_agg-wrapped exactly like `db_query`, and
/// return the same `HostDbResponse` JSON envelope (`{"data": [rows]}`). Used by
/// `host.submission.query_test_cases`.
pub(super) fn query_on_pool(
    db: &DatabaseConnection,
    plugin_id: &str,
    sql: String,
    args: String,
) -> Result<String, extism::Error> {
    let values = parse_args(&args)?;
    let wrapped_sql = format!(
        "SELECT COALESCE(json_agg(t), '[]'::json) AS json_data FROM ({}) AS t",
        sql
    );
    let stmt = plugin_statement(&wrapped_sql, values);

    let query_result = tokio::runtime::Handle::current().block_on(db.query_one_raw(stmt));

    match query_result {
        Ok(Some(res)) => {
            let json_val: serde_json::Value = res
                .try_get("", "json_data")
                .unwrap_or(serde_json::json!([]));
            HostDbResponse::ok(json_val).to_json_string()
        }
        Ok(None) => HostDbResponse::ok(serde_json::json!([])).to_json_string(),
        Err(e) => {
            error!(plugin_id, "Submission DB query error: {}", e);
            HostDbResponse::err(e.to_string()).to_json_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectOptions, Database};

    use super::*;

    fn permit() -> Arc<OwnedSemaphorePermit> {
        Arc::new(
            Arc::new(tokio::sync::Semaphore::new(1))
                .try_acquire_owned()
                .expect("a free permit"),
        )
    }

    #[test]
    fn role_escalation_guard_blocks_every_escape_vector() {
        for sql in [
            "RESET ROLE",
            // session-level SET of a host-enforced bound would persist on the
            // pooled connection (`SET LOCAL` is allowed, see the allow list)
            "SET statement_timeout = 0",
            "SET SESSION statement_timeout TO 0",
            "set statement_timeout to default",
            "SELECT 1; SET tcp_keepalives_idle = 0",
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
            // role escape, and the quoted body must stay visible to the guard
            "SELECT \"set_config\"('role','none',false)",
            "SELECT pg_catalog.\"set_config\"('role','none',false)",
            "SELECT \"pg_catalog\".\"set_config\"('role','none',false)",
            "SELECT COALESCE(json_agg(t), '[]') FROM (SELECT \"set_config\"('role','none',false)) AS t",
            "SET \"role\" = 'admin'",
            "SET \"session_authorization\" = 'admin'",
            // a standard string abutting a token that ends in `e` (name'..') is
            // NOT an E-string, so the lexer ends it at its close and still sees
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
                reject_session_escape("p", sql).is_err(),
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
            // a column named like an enforced setting is DML, not a SET command
            "UPDATE t SET statement_timeout = 5 WHERE id = 1",
            "RESET statement_timeout",
            // scope keyword before a NON-role parameter must stay allowed
            "SET LOCAL statement_timeout = 5000",
            "SET SESSION search_path TO my_schema",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            // role keywords or ';' that appear INSIDE a string literal are data,
            // not commands: the lexer neutralizes the literal so they must not
            // trip the guard
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
            // is valid and must not be mis-read as an E-string
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
                reject_session_escape("p", sql).is_ok(),
                "should have allowed: {sql}"
            );
        }
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
        let db: DatabaseConnection = Database::connect(opt).await.expect("connect").into();

        let txn = db.begin().await.expect("begin orphan txn");
        let txn_map: TransactionMap = Arc::new(StdMutex::new(HashMap::new()));
        let txn_id = "orphan-txn".to_string();
        txn_map.lock().unwrap().insert(
            txn_id.clone(),
            Arc::new(PluginTransaction::new(
                txn,
                "trapped-plugin".to_string(),
                permit(),
            )),
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
        let db: DatabaseConnection = Database::connect(url).await.expect("connect").into();

        let txn = db.begin().await.expect("begin");
        let mut map: HashMap<String, Arc<PluginTransaction>> = HashMap::new();
        map.insert(
            "txn-1".to_string(),
            Arc::new(PluginTransaction::new(txn, "owner".to_string(), permit())),
        );

        let foreign = lookup_owned_txn(&map, "txn-1", "intruder", "db_query_in")
            .expect_err("foreign txn id must be rejected");
        assert_eq!(
            foreign.to_string(),
            "Transaction not found: txn-1",
            "foreign txn must be indistinguishable from a missing one"
        );
        let missing = lookup_owned_txn(&map, "txn-2", "intruder", "db_query_in")
            .expect_err("missing txn id must be rejected");
        assert_eq!(missing.to_string(), "Transaction not found: txn-2");

        assert!(
            lookup_owned_txn(&map, "txn-1", "owner", "db_query_in").is_ok(),
            "owner keeps access to its own transaction"
        );
        assert!(
            map.contains_key("txn-1"),
            "rejected lookups must not remove the entry"
        );
    }

    /// A slow statement inside one plugin's transaction must not hold up a
    /// statement inside another's: the server-wide map lock is held only for
    /// the lookup, never across a transaction's DB call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_statement_in_one_transaction_does_not_block_another() {
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
        let db: DatabaseConnection = Database::connect(url).await.expect("connect").into();

        let txn_map: TransactionMap = Arc::new(StdMutex::new(HashMap::new()));
        for (id, owner) in [("slow", "plugin-a"), ("fast", "plugin-b")] {
            let txn = db.begin().await.expect("begin");
            txn_map.lock().unwrap().insert(
                id.to_string(),
                Arc::new(PluginTransaction::new(txn, owner.to_string(), permit())),
            );
        }

        // Host fns run on blocking threads and block_on the runtime.
        let run =
            |txn_map: TransactionMap, id: &'static str, owner: &'static str, sql: &'static str| {
                tokio::task::spawn_blocking(move || {
                    with_owned_txn(&txn_map, id, owner, "db_execute_in", |txn| {
                        tokio::runtime::Handle::current()
                            .block_on(txn.execute_unprepared(sql))
                            .expect("statement")
                    })
                    .expect("owned transaction")
                })
            };

        let slow = run(txn_map.clone(), "slow", "plugin-a", "SELECT pg_sleep(3)");
        // Let the slow statement start and take its transaction's lock.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        run(txn_map.clone(), "fast", "plugin-b", "SELECT 1")
            .await
            .expect("fast statement task");
        let fast_elapsed = started.elapsed();
        assert!(
            !slow.is_finished(),
            "the slow statement must still be running for this to prove anything"
        );
        assert!(
            fast_elapsed < Duration::from_secs(2),
            "a statement in another transaction waited {fast_elapsed:?} behind the slow one"
        );
        slow.await.expect("slow statement task");
    }

    /// One plugin statement bound with values of every JSON type in turn, on
    /// one connection: each execution binds its own types, whatever an earlier
    /// execution of the same SQL on that connection bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_statement_binds_each_json_type_on_one_connection() {
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
        let db: DatabaseConnection = Database::connect(opt).await.expect("connect").into();
        db.execute_unprepared("CREATE TABLE kv (v text)")
            .await
            .expect("create");

        let sql = "INSERT INTO kv (v) VALUES ($1)";
        for args in [
            r#"["a"]"#,
            "[42]",
            "[false]",
            "[1.5]",
            r#"[{"k":1}]"#,
            "[null]",
        ] {
            let values = parse_args(args).unwrap();
            db.execute_raw(plugin_statement(sql, values))
                .await
                .unwrap_or_else(|e| panic!("binding {args} after an earlier type: {e}"));
        }
        let stored: Vec<Option<String>> = db
            .query_all_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT v FROM kv ORDER BY ctid",
            ))
            .await
            .expect("select")
            .iter()
            .map(|row| row.try_get("", "v").unwrap())
            .collect();
        assert_eq!(
            stored,
            [
                Some("a"),
                Some("42"),
                Some("false"),
                Some("1.5"),
                Some(r#"{"k": 1}"#),
                None
            ]
            .map(|v| v.map(str::to_string))
        );
    }
}
