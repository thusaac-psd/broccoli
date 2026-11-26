//! While the database cannot be reached, every API call answers a retryable
//! `503 DATABASE_UNAVAILABLE` with `Retry-After`, the way `/healthz` reports
//! it, so clients wait and retry instead of reporting a server bug.

use serde_json::json;

use crate::common::{TestApp, routes};

fn assert_unavailable(what: &str, res: &crate::common::TestResponse) {
    assert_eq!(res.status, 503, "{what}: {}", res.text);
    assert_eq!(
        res.body["code"], "DATABASE_UNAVAILABLE",
        "{what}: {}",
        res.text
    );
    assert_eq!(
        res.headers.get("retry-after").and_then(|v| v.to_str().ok()),
        Some(
            server::error::DATABASE_RETRY_AFTER_SECS
                .to_string()
                .as_str()
        ),
        "{what}"
    );
}

#[tokio::test]
async fn api_calls_answer_a_database_outage_as_retryable() {
    let app = TestApp::spawn().await;
    let token = app.create_authenticated_user("dana", "securepass").await;
    let cli = app
        .post_with_token(routes::CLI_TOKEN, &json!({}), &token)
        .await;
    assert_eq!(cli.status, 200, "{}", cli.text);
    let refresh_token = cli.body["refresh_token"].as_str().unwrap().to_string();

    // Every clone shares one pool, so this takes the server's DB away.
    app.db.clone().close().await.expect("close pool");

    assert_unavailable(
        "contest list",
        &app.get_with_token("/api/v1/contests", &token).await,
    );
    assert_unavailable("me", &app.get_with_token(routes::ME, &token).await);
    assert_unavailable(
        "cli refresh",
        &app.post_without_token(
            routes::CLI_REFRESH,
            &json!({ "refresh_token": refresh_token }),
        )
        .await,
    );
}

/// A pool whose every connection is held by an open transaction is busy, not
/// lost: the request that waited out the acquire timeout answers as
/// unavailable, but the next ones still wait for a connection rather than
/// being refused, and are served once one comes free.
#[tokio::test]
async fn a_saturated_pool_is_not_taken_for_an_unreachable_database() {
    use common::db::{PgPoolSpec, PgSession};
    use futures::FutureExt;
    use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

    let database =
        test_support::create_test_database(&format!("saturation_{}", std::process::id())).await;
    let spec = PgPoolSpec {
        acquire_timeout: std::time::Duration::from_secs(2),
        ..PgPoolSpec::new(2, 0)
    };
    let db = common::db::connect(database.url(), &spec, &PgSession::new("test", 0, true))
        .await
        .unwrap();
    let select = || Statement::from_string(DbBackend::Postgres, "SELECT 1");

    let first = db.begin().await.unwrap();
    let second = db.begin().await.unwrap();
    let timed_out = db.query_one_raw(select()).await.unwrap_err();
    assert_eq!(
        common::db::outage(&timed_out),
        Some(common::db::Outage::Busy),
        "{timed_out}"
    );

    // Were the database taken for unreachable, one of these would probe and
    // the other be refused at once.
    let mut next = Box::pin(db.query_one_raw(select()));
    let mut after = Box::pin(db.query_one_raw(select()));
    for (name, waiting) in [("next", &mut next), ("after", &mut after)] {
        assert!(
            waiting.now_or_never().is_none(),
            "the {name} request waits for a connection instead of being refused"
        );
    }
    first.commit().await.unwrap();
    next.await.expect("served once a connection comes free");
    after.await.expect("served once a connection comes free");
    second.rollback().await.unwrap();
    db.close().await.unwrap();
}

/// Wait until `n` sessions of the test database wait for a lock of `locktype`,
/// read on `observer`'s own connection.
async fn until_waiting(observer: &common::db::DatabaseConnection, locktype: &str, n: i64) {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let waiting = async {
        loop {
            let waiting: i64 = observer
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT count(*) AS waiting FROM pg_locks \
                     WHERE locktype = $1 AND NOT granted \
                       AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
                    [locktype.into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "waiting")
                .unwrap();
            if waiting >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), waiting)
        .await
        .expect("the statements queue on the lock");
}

/// A pool of `max` connections on `database` that gives up on an acquire
/// after a second.
async fn small_pool(
    database: &test_support::TestDatabase,
    max: u32,
) -> common::db::DatabaseConnection {
    use common::db::{PgPoolSpec, PgSession};
    let spec = PgPoolSpec {
        acquire_timeout: std::time::Duration::from_secs(1),
        ..PgPoolSpec::new(max, 0)
    };
    common::db::connect(database.url(), &spec, &PgSession::new("test", 0, true))
        .await
        .unwrap()
}

/// With every connection of `db` checked out by a running statement, the
/// request that waits out the acquire timeout answers as unavailable, and the
/// next ones still wait for a connection instead of being refused.
async fn assert_busy_not_lost(
    db: &common::db::DatabaseConnection,
) -> Vec<
    std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<sea_orm::QueryResult>, sea_orm::DbErr>>
                + Send,
        >,
    >,
> {
    use futures::FutureExt;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let select = || Statement::from_string(DbBackend::Postgres, "SELECT 1");

    let timed_out = db.query_one_raw(select()).await.unwrap_err();
    assert_eq!(
        common::db::outage(&timed_out),
        Some(common::db::Outage::Busy),
        "{timed_out}"
    );

    // Were the database taken for unreachable, one of these would probe and
    // the other be refused at once.
    let mut waiting = Vec::new();
    for name in ["next", "after"] {
        let db = db.clone();
        let mut request: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>> =
            Box::pin(async move { db.query_one_raw(select()).await });
        assert!(
            (&mut request).now_or_never().is_none(),
            "the {name} request waits for a connection instead of being refused"
        );
        waiting.push(request);
    }
    waiting
}

/// One standalone statement running on a pool of one connection keeps it
/// busy: the pool is saturated, not lost.
#[tokio::test]
async fn a_running_statement_keeps_a_pool_of_one_busy_not_lost() {
    use sea_orm::{ConnectionTrait, TransactionTrait};

    let database =
        test_support::create_test_database(&format!("busy_one_{}", std::process::id())).await;
    let observer = small_pool(&database, 2).await;
    let db = small_pool(&database, 1).await;

    let blocker = observer.begin().await.unwrap();
    blocker
        .execute_unprepared("SELECT pg_advisory_xact_lock(42)")
        .await
        .unwrap();
    let running = tokio::spawn({
        let db = db.clone();
        async move {
            db.execute_unprepared("SELECT pg_advisory_xact_lock(42)")
                .await
        }
    });
    until_waiting(&observer, "advisory", 1).await;

    let waiting = assert_busy_not_lost(&db).await;
    blocker.rollback().await.unwrap();
    running
        .await
        .unwrap()
        .expect("the running statement finishes");
    for request in waiting {
        request
            .await
            .expect("served once the connection comes free");
    }
    db.close().await.unwrap();
    observer.close().await.unwrap();
}

/// Requests queued on one table's lock hold every connection, and a request
/// for another table waits out the acquire timeout: that request answers as
/// unavailable, the API stays up for the others.
#[tokio::test]
async fn statements_queued_on_a_lock_keep_the_pool_busy_not_lost() {
    use sea_orm::{ConnectionTrait, TransactionTrait};

    let database =
        test_support::create_test_database(&format!("busy_lock_{}", std::process::id())).await;
    let observer = small_pool(&database, 2).await;
    let db = small_pool(&database, 3).await;
    observer
        .execute_unprepared("CREATE TABLE locked (id int)")
        .await
        .unwrap();

    let blocker = observer.begin().await.unwrap();
    blocker
        .execute_unprepared("LOCK TABLE locked IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let queued: Vec<_> = (0..3)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { db.execute_unprepared("SELECT * FROM locked").await })
        })
        .collect();
    until_waiting(&observer, "relation", 3).await;

    let waiting = assert_busy_not_lost(&db).await;
    blocker.rollback().await.unwrap();
    for read in queued {
        read.await.unwrap().expect("each queued read finishes");
    }
    for request in waiting {
        request.await.expect("served once a connection comes free");
    }
    db.close().await.unwrap();
    observer.close().await.unwrap();
}
