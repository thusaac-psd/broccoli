//! Every statement a database handle runs reaches its metric callback once,
//! whichever way it runs: on a pooled connection the handle checks out, in a
//! transaction, prepared or unprepared.

use std::sync::{Arc, Mutex};

use common::db::{PgPoolSpec, PgSession};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_statement_reaches_the_metric_callback_once() {
    let database =
        test_support::create_test_database(&format!("metric_{}", std::process::id())).await;
    let mut db = common::db::connect(
        database.url(),
        &PgPoolSpec::new(2, 0),
        &PgSession::unbounded("test"),
    )
    .await
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let record = Arc::clone(&seen);
    db.set_metric_callback(move |info| record.lock().unwrap().push(info.statement.sql.clone()));
    let stmt = |sql: &str| Statement::from_string(DbBackend::Postgres, sql);

    db.execute_unprepared("CREATE TEMP TABLE IF NOT EXISTS m (n int)")
        .await
        .unwrap();
    db.execute_raw(stmt("SELECT 1")).await.unwrap();
    db.query_one_raw(stmt("SELECT 2")).await.unwrap();
    db.query_all_raw(stmt("SELECT 3")).await.unwrap();

    let txn = db.begin().await.unwrap();
    txn.execute_unprepared("SELECT 4").await.unwrap();
    txn.execute_raw(stmt("SELECT 5")).await.unwrap();
    txn.query_one_raw(stmt("SELECT 6")).await.unwrap();
    txn.query_all_raw(stmt("SELECT 7")).await.unwrap();
    txn.commit().await.unwrap();

    let mut conn = db.acquire().await.unwrap();
    conn.execute_unprepared("SELECT 8").await.unwrap();
    conn.execute(stmt("SELECT 9")).await.unwrap();
    conn.query_one(stmt("SELECT 10")).await.unwrap();
    conn.query_all(stmt("SELECT 11")).await.unwrap();
    drop(conn);

    let mut expected = vec!["CREATE TEMP TABLE IF NOT EXISTS m (n int)".to_owned()];
    expected.extend((1..=11).map(|n| format!("SELECT {n}")));
    assert_eq!(*seen.lock().unwrap(), expected);
}
