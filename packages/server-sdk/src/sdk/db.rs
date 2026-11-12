use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use crate::error::SdkError;

pub struct Db {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: DbMock,
}

/// Serializes [`Db::ensure_schema`] across every replica and plugin. The
/// two-key advisory lock form never conflicts with the host's single-key locks.
const SCHEMA_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtext('broccoli-plugin-schema'), hashtext($1))";

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use std::cell::Cell;

    use crate::sdk::shared::{HostDbResponse, parse_affected, parse_rows};

    impl Db {
        pub fn query<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>, SdkError> {
            self.query_with_args(sql, &[] as &[JsonValue])
        }

        pub fn query_with_args<T: DeserializeOwned>(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<Vec<T>, SdkError> {
            let args_json = serde_json::to_string(args)?;
            let result_json = unsafe { crate::host::raw::db_query(sql.to_string(), args_json)? };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            parse_rows(resp.into_result()?)
        }

        pub fn query_one<T: DeserializeOwned>(&self, sql: &str) -> Result<Option<T>, SdkError> {
            self.query_one_with_args(sql, &[] as &[JsonValue])
        }

        pub fn query_one_with_args<T: DeserializeOwned>(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<Option<T>, SdkError> {
            let mut rows: Vec<T> = self.query_with_args(sql, args)?;
            Ok(if rows.is_empty() {
                None
            } else {
                Some(rows.swap_remove(0))
            })
        }

        pub fn execute(&self, sql: &str) -> Result<u64, SdkError> {
            self.execute_with_args(sql, &[] as &[JsonValue])
        }

        pub fn execute_with_args(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<u64, SdkError> {
            let args_json = serde_json::to_string(args)?;
            let result_json = unsafe { crate::host::raw::db_execute(sql.to_string(), args_json)? };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            Ok(parse_affected(resp.into_result()?))
        }

        pub fn begin(&self) -> Result<Transaction, SdkError> {
            let result_json = unsafe { crate::host::raw::db_begin("{}".to_string())? };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            let data = resp.into_result()?;

            #[derive(serde::Deserialize)]
            struct BeginResponse {
                txn_id: String,
            }
            let begin: BeginResponse = serde_json::from_value(
                data.ok_or_else(|| SdkError::Database("Empty begin response".into()))?,
            )?;
            Ok(Transaction {
                id: begin.txn_id,
                finished: Cell::new(false),
            })
        }

        /// Run a plugin's idempotent schema statements (`CREATE TABLE IF NOT
        /// EXISTS ...`) in one transaction that first takes an advisory lock
        /// keyed by `name`. Postgres does not serialize concurrent `IF NOT
        /// EXISTS` DDL, so replicas loading the plugin at once would otherwise
        /// race on the catalog and one `init` would fail. The lock makes later
        /// callers wait for the first and then find every object present.
        pub fn ensure_schema(&self, name: &str, statements: &[&str]) -> Result<(), SdkError> {
            let txn = self.begin()?;
            txn.execute_with_args(SCHEMA_LOCK_SQL, &[name])?;
            for statement in statements {
                txn.execute(statement)?;
            }
            txn.commit()
        }
    }

    pub struct Transaction {
        id: String,
        finished: Cell<bool>,
    }

    impl Transaction {
        pub fn query<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>, SdkError> {
            self.query_with_args(sql, &[] as &[JsonValue])
        }

        pub fn query_with_args<T: DeserializeOwned>(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<Vec<T>, SdkError> {
            let args_json = serde_json::to_string(args)?;
            let result_json = unsafe {
                crate::host::raw::db_query_in(self.id.clone(), sql.to_string(), args_json)?
            };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            parse_rows(resp.into_result()?)
        }

        pub fn query_one<T: DeserializeOwned>(&self, sql: &str) -> Result<Option<T>, SdkError> {
            self.query_one_with_args(sql, &[] as &[JsonValue])
        }

        pub fn query_one_with_args<T: DeserializeOwned>(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<Option<T>, SdkError> {
            let mut rows: Vec<T> = self.query_with_args(sql, args)?;
            Ok(if rows.is_empty() {
                None
            } else {
                Some(rows.swap_remove(0))
            })
        }

        pub fn execute(&self, sql: &str) -> Result<u64, SdkError> {
            self.execute_with_args(sql, &[] as &[JsonValue])
        }

        pub fn execute_with_args(
            &self,
            sql: &str,
            args: &[impl serde::Serialize],
        ) -> Result<u64, SdkError> {
            let args_json = serde_json::to_string(args)?;
            let result_json = unsafe {
                crate::host::raw::db_execute_in(self.id.clone(), sql.to_string(), args_json)?
            };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            Ok(parse_affected(resp.into_result()?))
        }

        pub fn commit(self) -> Result<(), SdkError> {
            self.finished.set(true);
            let result_json = unsafe { crate::host::raw::db_commit(self.id.clone())? };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            resp.into_result()?;
            Ok(())
        }

        pub fn rollback(self) -> Result<(), SdkError> {
            self.finished.set(true);
            let result_json = unsafe { crate::host::raw::db_rollback(self.id.clone())? };
            let resp: HostDbResponse = serde_json::from_str(&result_json)?;
            resp.into_result()?;
            Ok(())
        }
    }

    impl Drop for Transaction {
        fn drop(&mut self) {
            if !self.finished.get() {
                let _ = unsafe { crate::host::raw::db_rollback(self.id.clone()) };
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::Transaction;

#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct DbMock {
    query_results: RefCell<VecDeque<Result<JsonValue, SdkError>>>,
    execute_results: RefCell<VecDeque<Result<u64, SdkError>>>,
    queries: RefCell<Vec<RecordedQuery>>,
    executions: RefCell<Vec<RecordedExecution>>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct RecordedQuery {
    pub sql: String,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct RecordedExecution {
    pub sql: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl DbMock {
    pub fn new() -> Self {
        Self {
            query_results: RefCell::new(VecDeque::new()),
            execute_results: RefCell::new(VecDeque::new()),
            queries: RefCell::new(Vec::new()),
            executions: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Transaction {
    _id: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Transaction {
    pub fn query<T: DeserializeOwned>(&self, _sql: &str) -> Result<Vec<T>, SdkError> {
        Ok(Vec::new())
    }

    pub fn query_with_args<T: DeserializeOwned>(
        &self,
        _sql: &str,
        _args: &[impl serde::Serialize],
    ) -> Result<Vec<T>, SdkError> {
        Ok(Vec::new())
    }

    pub fn query_one<T: DeserializeOwned>(&self, _sql: &str) -> Result<Option<T>, SdkError> {
        Ok(None)
    }

    pub fn query_one_with_args<T: DeserializeOwned>(
        &self,
        _sql: &str,
        _args: &[impl serde::Serialize],
    ) -> Result<Option<T>, SdkError> {
        Ok(None)
    }

    pub fn execute(&self, _sql: &str) -> Result<u64, SdkError> {
        Ok(0)
    }

    pub fn execute_with_args(
        &self,
        _sql: &str,
        _args: &[impl serde::Serialize],
    ) -> Result<u64, SdkError> {
        Ok(0)
    }

    pub fn commit(self) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn rollback(self) -> Result<(), SdkError> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Db {
    pub fn query<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>, SdkError> {
        self.inner
            .queries
            .borrow_mut()
            .push(RecordedQuery { sql: sql.into() });
        if let Some(result) = self.inner.query_results.borrow_mut().pop_front() {
            return match result {
                Ok(v) => Ok(serde_json::from_value(v)?),
                Err(e) => Err(e),
            };
        }
        Ok(Vec::new())
    }

    pub fn query_with_args<T: DeserializeOwned>(
        &self,
        sql: &str,
        _args: &[impl serde::Serialize],
    ) -> Result<Vec<T>, SdkError> {
        self.query(sql)
    }

    pub fn query_one<T: DeserializeOwned>(&self, sql: &str) -> Result<Option<T>, SdkError> {
        let mut rows: Vec<T> = self.query(sql)?;
        Ok(if rows.is_empty() {
            None
        } else {
            Some(rows.swap_remove(0))
        })
    }

    pub fn query_one_with_args<T: DeserializeOwned>(
        &self,
        sql: &str,
        args: &[impl serde::Serialize],
    ) -> Result<Option<T>, SdkError> {
        let mut rows: Vec<T> = self.query_with_args(sql, args)?;
        Ok(if rows.is_empty() {
            None
        } else {
            Some(rows.swap_remove(0))
        })
    }

    pub fn execute(&self, sql: &str) -> Result<u64, SdkError> {
        self.inner
            .executions
            .borrow_mut()
            .push(RecordedExecution { sql: sql.into() });
        if let Some(result) = self.inner.execute_results.borrow_mut().pop_front() {
            return result;
        }
        Ok(0)
    }

    pub fn execute_with_args(
        &self,
        sql: &str,
        _args: &[impl serde::Serialize],
    ) -> Result<u64, SdkError> {
        self.execute(sql)
    }

    pub fn begin(&self) -> Result<Transaction, SdkError> {
        Ok(Transaction {
            _id: "mock-txn".to_string(),
        })
    }

    /// Records the advisory lock and each statement as executions, in order.
    pub fn ensure_schema(&self, name: &str, statements: &[&str]) -> Result<(), SdkError> {
        self.execute_with_args(SCHEMA_LOCK_SQL, &[name])?;
        for statement in statements {
            self.execute(statement)?;
        }
        Ok(())
    }

    pub fn queue_query_result(&self, rows: JsonValue) {
        self.inner.query_results.borrow_mut().push_back(Ok(rows));
    }

    pub fn queue_query_error(&self, err: SdkError) {
        self.inner.query_results.borrow_mut().push_back(Err(err));
    }

    pub fn queue_execute_result(&self, affected: u64) {
        self.inner
            .execute_results
            .borrow_mut()
            .push_back(Ok(affected));
    }

    pub fn queue_execute_error(&self, err: SdkError) {
        self.inner.execute_results.borrow_mut().push_back(Err(err));
    }

    pub fn queries(&self) -> Vec<RecordedQuery> {
        self.inner.queries.borrow().clone()
    }

    pub fn executions(&self) -> Vec<RecordedExecution> {
        self.inner.executions.borrow().clone()
    }
}
