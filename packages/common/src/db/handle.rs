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

use std::backtrace::Backtrace;
use std::future::Future;
use std::pin::Pin;

use broccoli_types::types::sanitize_text_field;
use sea_orm::sea_query::Value;
use sea_orm::{
    AccessMode, ConnectionTrait, DbBackend, DbErr, ExecResult, IsolationLevel, QueryResult,
    Statement, TransactionError, TransactionOptions, TransactionSession, TransactionTrait,
};
use serde_json::Value as Json;

/// A pooled database connection whose statements are NUL-free (see the
/// module docs). Clones share the pool.
#[derive(Clone, Debug)]
pub struct DatabaseConnection(sea_orm::DatabaseConnection);

/// A transaction opened from a [`DatabaseConnection`], with the same guarantee.
#[derive(Debug)]
pub struct DatabaseTransaction(sea_orm::DatabaseTransaction);

impl From<sea_orm::DatabaseConnection> for DatabaseConnection {
    fn from(inner: sea_orm::DatabaseConnection) -> Self {
        Self(inner)
    }
}

impl DatabaseConnection {
    /// The underlying sea-orm connection, for APIs that take it by concrete
    /// type (schema migrations). Statements run on it skip the NUL check.
    pub fn as_sea_orm(&self) -> &sea_orm::DatabaseConnection {
        &self.0
    }

    /// Close the pool.
    pub async fn close(self) -> Result<(), DbErr> {
        self.0.close().await
    }

    /// Check that the database answers.
    pub async fn ping(&self) -> Result<(), DbErr> {
        self.0.ping().await
    }
}

impl DatabaseTransaction {
    /// Commit the transaction.
    pub async fn commit(self) -> Result<(), DbErr> {
        self.0.commit().await
    }

    /// Roll the transaction back.
    pub async fn rollback(self) -> Result<(), DbErr> {
        self.0.rollback().await
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
                self.0.execute_raw(without_nul(stmt)).await
            }

            async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
                if sql.contains('\0') {
                    report(sql, 1);
                    return self.0.execute_unprepared(&sanitize_text_field(sql)).await;
                }
                self.0.execute_unprepared(sql).await
            }

            async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
                self.0.query_one_raw(without_nul(stmt)).await
            }

            async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
                self.0.query_all_raw(without_nul(stmt)).await
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
                self.0.begin().await.map(DatabaseTransaction)
            }

            async fn begin_with_config(
                &self,
                isolation_level: Option<IsolationLevel>,
                access_mode: Option<AccessMode>,
            ) -> Result<DatabaseTransaction, DbErr> {
                self.0
                    .begin_with_config(isolation_level, access_mode)
                    .await
                    .map(DatabaseTransaction)
            }

            async fn begin_with_options(
                &self,
                options: TransactionOptions,
            ) -> Result<DatabaseTransaction, DbErr> {
                self.0
                    .begin_with_options(options)
                    .await
                    .map(DatabaseTransaction)
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
        self.0.commit().await
    }

    async fn rollback(self) -> Result<(), DbErr> {
        self.0.rollback().await
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

    #[test]
    fn nul_in_statement_text_becomes_a_replacement_character() {
        let stmt = without_nul(Statement::from_string(
            DbBackend::Postgres,
            "SELECT 'a\u{0}'",
        ));
        assert_eq!(stmt.sql, "SELECT 'a\u{FFFD}'");
    }
}
