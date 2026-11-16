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
    let db = common::db::connect(database.url(), &spec, &PgSession::unbounded("test"))
        .await
        .unwrap();
    let select = || Statement::from_string(DbBackend::Postgres, "SELECT 1");

    let first = db.begin().await.unwrap();
    let second = db.begin().await.unwrap();
    let timed_out = db.query_one_raw(select()).await.unwrap_err();
    assert!(common::db::is_outage(&timed_out), "{timed_out}");

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
