//! The handle every server and worker statement runs through, and the one
//! owner of NUL (U+0000) in text bound to Postgres.
//!
//! TEXT rejects U+0000 (SQLSTATE 22021), JSONB rejects `\u0000` (22P05), and a
//! NUL inside statement text cuts the protocol's NUL-terminated query string
//! short. Text a person submits is rejected at the HTTP edge with a 400; text a
//! machine produces (program, compiler, checker and plugin output) reaches the
//! database as is. Before a statement runs, this handle replaces every NUL in
//! its SQL and its bound strings, chars, JSON and arrays with U+FFFD, and logs
//! the statement and the call stack, so no statement fails on a NUL and no
//! unsanitized source goes unnoticed.
//!
//! It is also the one owner of fail-fast on an outage. Every connection the
//! handle uses is checked out through it: a standalone statement runs on a
//! connection the handle checks out for it, a transaction holds the one it
//! began on, and a caller that needs a backend of its own holds a
//! [`PooledConnection`]. Each checkout passes the pool's [`Reachability`] and
//! counts as busy until it is returned, and each statement's outcome settles
//! there by one rule, so while the database cannot be reached a caller is
//! refused at once, and a pool that is only busy is never taken for one that
//! is lost.
//!
//! Every statement reaches the metric callback once: sea-orm reports a
//! transaction's prepared statements, and the handle reports the rest (the
//! statements it runs on a [`PooledConnection`] and every unprepared one,
//! which sea-orm runs unreported).
//!
//! With the `test-hooks` feature only, a handle may wrap sea-orm's mock
//! connection (`MockDatabase`) for unit tests; a production build has no
//! mock path.

use std::backtrace::Backtrace;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use broccoli_types::types::sanitize_text_field;
use sea_orm::sea_query::Value;
use sea_orm::sqlx::Connection as _;
use sea_orm::{
    AccessMode, ConnAcquireErr, ConnectionTrait, DbBackend, DbErr, ExecResult, IsolationLevel,
    QueryResult, RuntimeErr, Statement, TransactionError, TransactionOptions, TransactionSession,
    TransactionTrait,
};
use serde_json::Value as Json;

use super::reachability::{Held, Reachability};

/// A pooled database connection whose statements are NUL-free and fail fast
/// while the database is unreachable (see the module docs). Clones share the
/// pool and what it knows.
#[derive(Clone, Debug)]
pub struct DatabaseConnection(sea_orm::DatabaseConnection, Arc<Reachability>, Metric);

/// The callback [`DatabaseConnection::set_metric_callback`] installs. sea-orm
/// calls it for a transaction's statements; the handle calls it for the
/// statements it runs on a [`PooledConnection`], which bypass sea-orm's
/// executor, so every statement reaches it once.
#[derive(Clone, Default)]
struct Metric(Option<Arc<MetricCallback>>);

type MetricCallback = dyn Fn(&sea_orm::metric::Info<'_>) + Send + Sync;

impl std::fmt::Debug for Metric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Metric(set)"
        } else {
            "Metric(none)"
        })
    }
}

impl Metric {
    async fn observe<T>(
        &self,
        stmt: &Statement,
        op: impl Future<Output = Result<T, DbErr>>,
    ) -> Result<T, DbErr> {
        let Some(callback) = &self.0 else {
            return op.await;
        };
        let start = std::time::Instant::now();
        let outcome = op.await;
        callback(&sea_orm::metric::Info {
            elapsed: start.elapsed(),
            statement: stmt,
            failed: outcome.is_err(),
        });
        outcome
    }
}

/// A transaction opened from a [`DatabaseConnection`], with the same guarantee.
/// While it is open, the connection it holds counts as busy, not lost, in the
/// pool's record of whether the database can be reached.
#[derive(Debug)]
pub struct DatabaseTransaction(sea_orm::DatabaseTransaction, Held, Metric);

/// A connection checked out of a [`DatabaseConnection`]'s pool. The handle
/// runs each standalone statement on one; a caller that must know which
/// backend runs its statements (a plugin's own SQL, which the host cancels on
/// that backend at its call's deadline) holds one itself. Checking it out
/// fails fast while the database is unreachable; while it is held it counts
/// as busy, not lost. Its statements are NUL-free and settle as every
/// statement does. Dropping it returns the connection to the pool.
#[derive(Debug)]
pub struct PooledConnection {
    conn: sea_orm::sqlx::pool::PoolConnection<sea_orm::sqlx::Postgres>,
    held: Held,
    metric: Metric,
}

/// How many connections a pool has open, and how many of them are idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConnections {
    pub open: u32,
    pub idle: usize,
}

impl PooledConnection {
    /// Run `stmt`.
    pub async fn execute(&mut self, stmt: Statement) -> Result<ExecResult, DbErr> {
        let stmt = without_nul(stmt);
        let run = sqlx_query(&stmt).execute(&mut *self.conn);
        let outcome = self
            .metric
            .observe(&stmt, async {
                run.await.map(ExecResult::from).map_err(exec_err)
            })
            .await;
        self.settle(outcome)
    }

    /// Run `sql`, which may hold several statements, unprepared.
    pub async fn execute_unprepared(&mut self, sql: &str) -> Result<ExecResult, DbErr> {
        use sea_orm::sqlx::Executor as _;
        let sql = if sql.contains('\0') {
            report(sql, 1);
            sanitize_text_field(sql).into_owned()
        } else {
            sql.to_owned()
        };
        let stmt = Statement::from_string(DbBackend::Postgres, sql);
        let run = (&mut *self.conn).execute(sea_orm::sqlx::AssertSqlSafe(stmt.sql.clone()));
        let outcome = self
            .metric
            .observe(&stmt, async {
                run.await.map(ExecResult::from).map_err(exec_err)
            })
            .await;
        self.settle(outcome)
    }

    /// Run `stmt` and return its first row, if any.
    pub async fn query_one(&mut self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        let stmt = without_nul(stmt);
        let run = sqlx_query(&stmt).fetch_optional(&mut *self.conn);
        let outcome = self
            .metric
            .observe(&stmt, async {
                run.await
                    .map(|row| row.map(QueryResult::from))
                    .map_err(query_err)
            })
            .await;
        self.settle(outcome)
    }

    /// Run `stmt` and return every row.
    pub async fn query_all(&mut self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        let stmt = without_nul(stmt);
        let run = sqlx_query(&stmt).fetch_all(&mut *self.conn);
        let outcome = self
            .metric
            .observe(&stmt, async {
                run.await
                    .map(|rows| rows.into_iter().map(QueryResult::from).collect())
                    .map_err(query_err)
            })
            .await;
        self.settle(outcome)
    }

    /// Check that the backend answers.
    pub async fn ping(&mut self) -> Result<(), DbErr> {
        let outcome = self
            .conn
            .ping()
            .await
            .map_err(|e| DbErr::Conn(RuntimeErr::SqlxError(e.into())));
        self.settle(outcome)
    }

    /// The pid of the backend this connection is attached to, so the caller
    /// can cancel a statement of its own there. It is one round trip per
    /// checkout: sqlx keeps the backend's pid private and gives a pooled
    /// connection no identity or slot to keep it in between checkouts. The
    /// read is the caller's bookkeeping, not one of its statements, so it
    /// settles as every statement does but is not reported to the metric
    /// callback.
    pub async fn backend_pid(&mut self) -> Result<i32, DbErr> {
        let outcome = sea_orm::sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *self.conn)
            .await
            .map_err(query_err);
        self.settle(outcome)
    }

    /// The raw connection, for bulk work sea-orm has no statement for
    /// (`COPY`). Statements run on it skip the NUL rule and settle nothing;
    /// the connection still counts as busy while this is held.
    pub fn raw(&mut self) -> &mut sea_orm::sqlx::PgConnection {
        &mut self.conn
    }

    fn settle<T>(&self, outcome: Result<T, DbErr>) -> Result<T, DbErr> {
        self.held.settle(&outcome);
        outcome
    }
}

fn exec_err(e: sea_orm::sqlx::Error) -> DbErr {
    DbErr::Exec(RuntimeErr::SqlxError(e.into()))
}

fn query_err(e: sea_orm::sqlx::Error) -> DbErr {
    DbErr::Query(RuntimeErr::SqlxError(e.into()))
}

/// `stmt` as a sqlx query, bound as sea-orm binds a statement.
fn sqlx_query(
    stmt: &Statement,
) -> sea_orm::sqlx::query::Query<'_, sea_orm::sqlx::Postgres, sea_query_sqlx::SqlxValues> {
    let values = stmt.values.clone().unwrap_or(sea_orm::Values(Vec::new()));
    sea_orm::sqlx::query_with(
        sea_orm::sqlx::AssertSqlSafe(stmt.sql.as_str()),
        sea_query_sqlx::SqlxValues(values),
    )
}

impl From<sea_orm::DatabaseConnection> for DatabaseConnection {
    /// A handle on `inner`'s Postgres pool.
    fn from(inner: sea_orm::DatabaseConnection) -> Self {
        Self(inner, Arc::default(), Metric::default())
    }
}

/// Unit tests' sea-orm mock behind the handle: its statements run on the
/// mock as they are, NUL rule included. It has no pool, so nothing checks a
/// connection out and no outage is decided.
#[cfg(feature = "test-hooks")]
impl DatabaseConnection {
    fn as_mock(&self) -> Option<&sea_orm::DatabaseConnection> {
        self.0.is_mock_connection().then_some(&self.0)
    }
}

impl DatabaseConnection {
    /// The underlying sea-orm connection, for APIs that take it by concrete
    /// type (schema migrations). Statements run on it skip the NUL check and
    /// the pool's reachability.
    pub fn as_sea_orm(&self) -> &sea_orm::DatabaseConnection {
        &self.0
    }

    /// Close the pool.
    pub async fn close(self) -> Result<(), DbErr> {
        self.0.close().await
    }

    /// Check that the database answers.
    pub async fn ping(&self) -> Result<(), DbErr> {
        self.acquire().await?.ping().await
    }

    /// Call `callback` with every statement this handle runs, as sea-orm's
    /// `set_metric_callback` does.
    pub fn set_metric_callback<F>(&mut self, callback: F)
    where
        F: Fn(&sea_orm::metric::Info<'_>) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        let shared = Arc::clone(&callback);
        self.0.set_metric_callback(move |info| shared(info));
        self.2 = Metric(Some(callback));
    }

    /// Check a connection out of the pool (see [`PooledConnection`]).
    pub async fn acquire(&self) -> Result<PooledConnection, DbErr> {
        let pool = self.pool()?;
        self.checked_out(async { pool.acquire().await.map_err(acquire_err) })
            .await
            .map(|(conn, held)| PooledConnection {
                conn,
                held,
                metric: self.2.clone(),
            })
    }

    /// How many connections the pool has open, and how many are idle.
    pub fn connections(&self) -> PoolConnections {
        self.pool()
            .map_or(PoolConnections { open: 0, idle: 0 }, |pool| {
                PoolConnections {
                    open: pool.size(),
                    idle: pool.num_idle(),
                }
            })
    }

    /// Close one idle connection, if the pool has one; true when it did.
    pub async fn close_idle(&self) -> bool {
        let Some(conn) = self.pool().ok().and_then(|pool| pool.try_acquire()) else {
            return false;
        };
        if let Err(e) = conn.close().await {
            tracing::debug!(error = %e, "Closing an idle connection failed");
        }
        true
    }

    /// Run `op`, which checks a connection out of the pool, past the pool's
    /// [`Reachability`], and count what it checked out busy.
    async fn checked_out<T>(
        &self,
        op: impl Future<Output = Result<T, DbErr>>,
    ) -> Result<(T, Held), DbErr> {
        let pass = self.1.pass()?;
        let outcome = op.await;
        self.1.settle_acquire(pass, outcome)
    }

    /// The pool connections are checked out of.
    fn pool(&self) -> Result<&sea_orm::sqlx::PgPool, DbErr> {
        #[cfg(feature = "test-hooks")]
        if self.0.is_mock_connection() {
            return Err(DbErr::Custom("a mock connection has no pool".into()));
        }
        Ok(self.0.get_postgres_connection_pool())
    }

    async fn exec(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        #[cfg(feature = "test-hooks")]
        if let Some(mock) = self.as_mock() {
            return mock.execute_raw(without_nul(stmt)).await;
        }
        self.acquire().await?.execute(stmt).await
    }

    async fn unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        #[cfg(feature = "test-hooks")]
        if let Some(mock) = self.as_mock() {
            return mock.execute_unprepared(&sanitize_text_field(sql)).await;
        }
        self.acquire().await?.execute_unprepared(sql).await
    }

    async fn one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        #[cfg(feature = "test-hooks")]
        if let Some(mock) = self.as_mock() {
            return mock.query_one_raw(without_nul(stmt)).await;
        }
        self.acquire().await?.query_one(stmt).await
    }

    async fn all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        #[cfg(feature = "test-hooks")]
        if let Some(mock) = self.as_mock() {
            return mock.query_all_raw(without_nul(stmt)).await;
        }
        self.acquire().await?.query_all(stmt).await
    }

    async fn open(
        &self,
        begin: impl Future<Output = Result<sea_orm::DatabaseTransaction, DbErr>>,
    ) -> Result<DatabaseTransaction, DbErr> {
        self.checked_out(begin)
            .await
            .map(|(txn, held)| DatabaseTransaction(txn, held, self.2.clone()))
    }
}

impl DatabaseTransaction {
    /// Commit the transaction.
    pub async fn commit(self) -> Result<(), DbErr> {
        let outcome = self.0.commit().await;
        self.1.settle(&outcome);
        outcome
    }

    /// Roll the transaction back.
    pub async fn rollback(self) -> Result<(), DbErr> {
        let outcome = self.0.rollback().await;
        self.1.settle(&outcome);
        outcome
    }

    /// A transaction's statements run on the connection it holds.
    async fn settled<T>(&self, op: impl Future<Output = Result<T, DbErr>>) -> Result<T, DbErr> {
        let outcome = op.await;
        self.1.settle(&outcome);
        outcome
    }

    async fn exec(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        self.settled(self.0.execute_raw(without_nul(stmt))).await
    }

    async fn unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        if sql.contains('\0') {
            report(sql, 1);
        }
        let stmt = Statement::from_string(DbBackend::Postgres, sanitize_text_field(sql));
        let run = self.0.execute_unprepared(&stmt.sql);
        self.settled(self.2.observe(&stmt, run)).await
    }

    async fn one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.settled(self.0.query_one_raw(without_nul(stmt))).await
    }

    async fn all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.settled(self.0.query_all_raw(without_nul(stmt))).await
    }

    /// A nested transaction (a savepoint) shares the connection, which this
    /// transaction already counts busy.
    async fn open(
        &self,
        begin: impl Future<Output = Result<sea_orm::DatabaseTransaction, DbErr>>,
    ) -> Result<DatabaseTransaction, DbErr> {
        let txn = self.settled(begin).await?;
        Ok(DatabaseTransaction(txn, self.1.share(), self.2.clone()))
    }
}

/// The error a failed pool acquire answers, as sea-orm reports its own.
fn acquire_err(e: sea_orm::sqlx::Error) -> DbErr {
    match e {
        sea_orm::sqlx::Error::PoolTimedOut => DbErr::ConnectionAcquire(ConnAcquireErr::Timeout),
        sea_orm::sqlx::Error::PoolClosed => {
            DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed)
        }
        e => DbErr::Conn(RuntimeErr::SqlxError(e.into())),
    }
}

macro_rules! checked_connection {
    ($ty:ty) => {
        #[async_trait::async_trait]
        impl ConnectionTrait for $ty {
            fn get_database_backend(&self) -> DbBackend {
                self.0.get_database_backend()
            }

            async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
                self.exec(stmt).await
            }

            async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
                self.unprepared(sql).await
            }

            async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
                self.one(stmt).await
            }

            async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
                self.all(stmt).await
            }

            fn support_returning(&self) -> bool {
                self.0.support_returning()
            }

            fn is_mock_connection(&self) -> bool {
                self.0.is_mock_connection()
            }
        }

        #[async_trait::async_trait]
        impl TransactionTrait for $ty {
            type Transaction = DatabaseTransaction;

            async fn begin(&self) -> Result<DatabaseTransaction, DbErr> {
                self.open(self.0.begin()).await
            }

            async fn begin_with_config(
                &self,
                isolation_level: Option<IsolationLevel>,
                access_mode: Option<AccessMode>,
            ) -> Result<DatabaseTransaction, DbErr> {
                self.open(self.0.begin_with_config(isolation_level, access_mode))
                    .await
            }

            async fn begin_with_options(
                &self,
                options: TransactionOptions,
            ) -> Result<DatabaseTransaction, DbErr> {
                self.open(self.0.begin_with_options(options)).await
            }

            async fn transaction<F, T, E>(&self, callback: F) -> Result<T, TransactionError<E>>
            where
                F: for<'c> FnOnce(
                        &'c DatabaseTransaction,
                    )
                        -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
                    + Send,
                T: Send,
                E: std::fmt::Display + std::fmt::Debug + Send,
            {
                run_in(self.begin().await, callback).await
            }

            async fn transaction_with_config<F, T, E>(
                &self,
                callback: F,
                isolation_level: Option<IsolationLevel>,
                access_mode: Option<AccessMode>,
            ) -> Result<T, TransactionError<E>>
            where
                F: for<'c> FnOnce(
                        &'c DatabaseTransaction,
                    )
                        -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
                    + Send,
                T: Send,
                E: std::fmt::Display + std::fmt::Debug + Send,
            {
                run_in(
                    self.begin_with_config(isolation_level, access_mode).await,
                    callback,
                )
                .await
            }
        }
    };
}

checked_connection!(DatabaseConnection);
checked_connection!(DatabaseTransaction);

#[async_trait::async_trait]
impl TransactionSession for DatabaseTransaction {
    async fn commit(self) -> Result<(), DbErr> {
        DatabaseTransaction::commit(self).await
    }

    async fn rollback(self) -> Result<(), DbErr> {
        DatabaseTransaction::rollback(self).await
    }
}

/// Run `callback` in `txn`, committing on `Ok` and rolling back on `Err`.
async fn run_in<F, T, E>(
    txn: Result<DatabaseTransaction, DbErr>,
    callback: F,
) -> Result<T, TransactionError<E>>
where
    F: for<'c> FnOnce(
            &'c DatabaseTransaction,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
        + Send,
    T: Send,
    E: std::fmt::Display + std::fmt::Debug + Send,
{
    let txn = txn.map_err(TransactionError::Connection)?;
    match callback(&txn).await {
        Ok(value) => {
            txn.commit().await.map_err(TransactionError::Connection)?;
            Ok(value)
        }
        Err(e) => {
            txn.rollback().await.map_err(TransactionError::Connection)?;
            Err(TransactionError::Transaction(e))
        }
    }
}

/// `stmt` with every NUL in its SQL and bound values replaced by U+FFFD.
/// Scans each value once and allocates only when it finds a NUL.
fn without_nul(mut stmt: Statement) -> Statement {
    let mut replaced = 0;
    if stmt.sql.contains('\0') {
        stmt.sql = sanitize_text_field(&stmt.sql).into_owned();
        replaced += 1;
    }
    if let Some(values) = &mut stmt.values {
        replaced += values.0.iter_mut().map(value_without_nul).sum::<usize>();
    }
    if replaced > 0 {
        report(&stmt.sql, replaced);
    }
    stmt
}

/// Replace the NULs in one bound value; returns how many strings held one.
fn value_without_nul(value: &mut Value) -> usize {
    match value {
        Value::String(Some(s)) => string_without_nul(s),
        Value::Char(Some(c)) if *c == '\0' => {
            *c = '\u{FFFD}';
            1
        }
        Value::Json(Some(json)) => json_without_nul(json),
        Value::Array(_, Some(items)) => items.iter_mut().map(value_without_nul).sum(),
        _ => 0,
    }
}

fn string_without_nul(s: &mut String) -> usize {
    if s.contains('\0') {
        *s = sanitize_text_field(s).into_owned();
        1
    } else {
        0
    }
}

fn json_without_nul(json: &mut Json) -> usize {
    match json {
        Json::String(s) => string_without_nul(s),
        Json::Array(items) => items.iter_mut().map(json_without_nul).sum(),
        Json::Object(map) => {
            let mut replaced: usize = map.values_mut().map(json_without_nul).sum();
            if map.keys().any(|key| key.contains('\0')) {
                let entries = std::mem::take(map);
                for (key, value) in entries {
                    if key.contains('\0') {
                        replaced += 1;
                    }
                    map.insert(sanitize_text_field(&key).into_owned(), value);
                }
            }
            replaced
        }
        _ => 0,
    }
}

fn report(sql: &str, replaced: usize) {
    let sql: String = sql.chars().take(200).collect();
    tracing::warn!(
        %sql,
        replaced,
        call_stack = %Backtrace::force_capture(),
        "Replaced NUL in text bound to the database; the code that produced it \
         should reject or sanitize it where the text enters"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(values: Vec<Value>) -> Vec<Value> {
        without_nul(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT $1",
            values,
        ))
        .values
        .unwrap()
        .0
    }

    #[test]
    fn nul_in_every_text_shape_becomes_a_replacement_character() {
        let json = serde_json::json!({ "std\u{0}out": ["a\u{0}b", { "k": "x\u{0}" }], "n": 1 });
        let values = bound(vec![
            "a\u{0}b".into(),
            Value::Char(Some('\0')),
            Value::Json(Some(Box::new(json))),
            Value::Array(
                sea_orm::sea_query::ArrayType::String,
                Some(Box::new(vec!["x\u{0}".into(), "clean".into()])),
            ),
        ]);
        assert_eq!(values[0], Value::from("a\u{FFFD}b"));
        assert_eq!(values[1], Value::Char(Some('\u{FFFD}')));
        assert_eq!(
            values[2],
            Value::Json(Some(Box::new(serde_json::json!({
                "std\u{FFFD}out": ["a\u{FFFD}b", { "k": "x\u{FFFD}" }],
                "n": 1
            }))))
        );
        assert_eq!(
            values[3],
            Value::Array(
                sea_orm::sea_query::ArrayType::String,
                Some(Box::new(vec!["x\u{FFFD}".into(), "clean".into()])),
            )
        );
    }

    #[test]
    fn clean_statements_pass_through_unchanged() {
        let values = vec!["clean".into(), 7i64.into(), Value::String(None)];
        assert_eq!(bound(values.clone()), values);
    }

    /// While the database cannot be reached, a statement, a transaction or a
    /// checked-out connection waits for no connection: the one probe in
    /// flight waits, every other is refused at once as an outage.
    #[tokio::test]
    async fn statements_fail_fast_while_the_database_is_unreachable() {
        use std::task::Poll;

        // A database host that accepts connections and never answers.
        let blackhole = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = blackhole.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = blackhole.accept().await {
                held.push(socket);
            }
        });
        let url = format!("postgres://u:p@{addr}/db");
        let spec = super::super::PgPoolSpec {
            acquire_timeout: std::time::Duration::from_millis(200),
            ..super::super::PgPoolSpec::new(4, 0)
        };
        let db = super::super::connect_lazy_with(
            spec.pool_options(),
            super::super::pg_connect_options(&url, &super::super::PgSession::unbounded("test"))
                .unwrap(),
        );
        let select = || Statement::from_string(DbBackend::Postgres, "SELECT 1");

        let first = db.query_one_raw(select()).await.unwrap_err();
        assert!(super::super::is_outage(&first), "{first}");

        let mut probe = Box::pin(db.query_one_raw(select()));
        assert!(matches!(futures::poll!(&mut probe), Poll::Pending));
        let mut refused = Box::pin(db.execute_unprepared("SELECT 1"));
        match futures::poll!(&mut refused) {
            Poll::Ready(Err(e)) => assert!(super::super::is_outage(&e), "{e}"),
            other => panic!("refused at once, got {other:?}"),
        }
        let mut refused = Box::pin(db.begin());
        assert!(matches!(
            futures::poll!(&mut refused),
            Poll::Ready(Err(DbErr::ConnectionAcquire(_)))
        ));
        let mut refused = Box::pin(db.acquire());
        match futures::poll!(&mut refused) {
            Poll::Ready(Err(e)) => assert!(super::super::is_outage(&e), "{e}"),
            other => panic!("refused at once, got {:?}", other.map(|r| r.map(|_| ()))),
        }
        assert!(probe.await.is_err());
    }

    #[test]
    fn nul_in_statement_text_becomes_a_replacement_character() {
        let stmt = without_nul(Statement::from_string(
            DbBackend::Postgres,
            "SELECT 'a\u{0}'",
        ));
        assert_eq!(stmt.sql, "SELECT 'a\u{FFFD}'");
    }
}
