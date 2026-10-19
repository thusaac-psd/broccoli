use std::time::Duration;

use broccoli_server_sdk::types::PublishNotification;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use server::services::notifications;

use crate::common::{TestApp, routes};

struct Context {
    app: TestApp,
    contest: i32,
    user: i32,
    token: String,
    other: i32,
}

impl Context {
    async fn new() -> Self {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("staff", "test-password", "admin")
            .await;
        let contest = app
            .create_contest(&admin, "Notifications", true, false)
            .await;
        let token = app
            .create_authenticated_user("player", "test-password")
            .await;
        let user = app.get_with_token(routes::ME, &token).await.id();
        let other_token = app
            .create_authenticated_user("opponent", "test-password")
            .await;
        let other = app.get_with_token(routes::ME, &other_token).await.id();
        Self {
            app,
            contest,
            user,
            token,
            other,
        }
    }

    fn message(&self, key: &str) -> PublishNotification {
        PublishNotification {
            contest_id: self.contest,
            user_ids: vec![self.user, self.other],
            key: key.into(),
            kind: "game-ended".into(),
            payload: json!({"game_index": 0}),
        }
    }

    fn path(&self, after: Option<i64>) -> String {
        let mut path = format!(
            "/api/v1/contests/{}/notifications?plugin_id=codelink-bracket",
            self.contest
        );
        if let Some(after) = after {
            path.push_str(&format!("&after={after}"));
        }
        path
    }
}

pub(crate) async fn next_frame(response: &mut reqwest::Response, buffer: &mut String) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(end) = buffer.find("\n\n") {
                return buffer.drain(..end + 2).collect();
            }
            let chunk = response
                .chunk()
                .await
                .unwrap()
                .expect("SSE stream ended unexpectedly");
            buffer.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    })
    .await
    .expect("notification should arrive promptly")
}

#[tokio::test]
async fn notifications_are_idempotent_and_scoped_to_recipient_contest_and_plugin() {
    let ctx = Context::new().await;
    let first = ctx.message("game:0:0:100");
    notifications::publish(&ctx.app.db, "codelink-bracket", &first)
        .await
        .unwrap();
    // Renumbering the file during a rebase must preserve the recorded migration
    // identity, and restarting migration discovery must retain existing events.
    let applied = ctx
        .app
        .db
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM seaql_migrations WHERE version = 'm0011_plugin_notifications'",
        ))
        .await
        .unwrap();
    assert!(applied.is_some());
    server::migration::Migrator::up(&ctx.app.db, None)
        .await
        .unwrap();
    let mut duplicate = first.clone();
    duplicate.payload = json!({"unexpected": "replacement"});
    notifications::publish(&ctx.app.db, "codelink-bracket", &duplicate)
        .await
        .unwrap();
    let own = notifications::after(&ctx.app.db, ctx.contest, ctx.user, "codelink-bracket", 0)
        .await
        .unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].payload, first.payload);
    assert_eq!(
        notifications::after(&ctx.app.db, ctx.contest, ctx.other, "codelink-bracket", 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        notifications::after(&ctx.app.db, ctx.contest, 0, "codelink-bracket", 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        notifications::after(
            &ctx.app.db,
            ctx.contest + 1,
            ctx.user,
            "codelink-bracket",
            0
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        notifications::after(&ctx.app.db, ctx.contest, ctx.user, "another-plugin", 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        notifications::after(
            &ctx.app.db,
            ctx.contest,
            ctx.user,
            "codelink-bracket",
            own[0].id.parse().unwrap()
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn sse_starts_now_delivers_live_and_replays_only_the_authenticated_users_events() {
    let ctx = Context::new().await;
    notifications::publish(&ctx.app.db, "codelink-bracket", &ctx.message("old"))
        .await
        .unwrap();
    let mut response = ctx.app.download_raw(&ctx.path(None), &ctx.token).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    assert!(response.headers().get("content-encoding").is_none());
    let mut buffer = String::new();
    let ready = next_frame(&mut response, &mut buffer).await;
    assert!(ready.contains("event: ready"));
    let cursor: i64 = ready
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(cursor > 0);
    let mut opponent_only = ctx.message("opponent-only");
    opponent_only.user_ids = vec![ctx.other];
    notifications::publish(&ctx.app.db, "codelink-bracket", &opponent_only)
        .await
        .unwrap();
    notifications::publish(&ctx.app.db, "codelink-bracket", &ctx.message("live"))
        .await
        .unwrap();
    let live = next_frame(&mut response, &mut buffer).await;
    assert!(live.contains("event: notification"));
    assert!(live.contains("\"key\":\"live\""));
    assert!(!live.contains("opponent-only"));
    drop(response);
    let mut replay = ctx
        .app
        .download_raw(&ctx.path(Some(cursor)), &ctx.token)
        .await;
    buffer.clear();
    assert!(
        next_frame(&mut replay, &mut buffer)
            .await
            .contains("event: ready")
    );
    assert_eq!(next_frame(&mut replay, &mut buffer).await, live);
}

#[tokio::test]
async fn notification_stream_requires_auth_and_contest_access() {
    let ctx = Context::new().await;
    let schema = ctx.app.get_without_token("/api-docs/openapi.json").await;
    let params = schema.body["paths"]["/contests/{id}/notifications"]["get"]["parameters"]
        .as_array()
        .unwrap();
    for name in ["plugin_id", "after"] {
        let param = params.iter().find(|param| param["name"] == name).unwrap();
        assert_eq!(
            param["in"], "query",
            "SDK must send {name} in the query, not the path"
        );
    }
    assert_eq!(ctx.app.get_without_token(&ctx.path(None)).await.status, 401);
    let admin = ctx
        .app
        .create_user_with_role("private_staff", "test-password", "admin")
        .await;
    let hidden = ctx
        .app
        .create_contest(&admin, "Private", false, false)
        .await;
    let path = format!("/api/v1/contests/{hidden}/notifications?plugin_id=codelink-bracket");
    assert_eq!(ctx.app.get_with_token(&path, &ctx.token).await.status, 404);
    assert_eq!(
        ctx.app
            .get_with_token(&ctx.path(Some(-1)), &ctx.token)
            .await
            .status,
        400
    );
}

#[tokio::test]
async fn recipient_commit_order_prevents_skipping_a_slow_event() {
    let ctx = Context::new().await;
    let txn = ctx.app.db.begin().await.unwrap();
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("notification:{}:{}", ctx.contest, ctx.user).into()],
    ))
    .await
    .unwrap();
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO plugin_notification (plugin_id, contest_id, user_id, event_key, kind, payload)
         VALUES ('codelink-bracket', $1, $2, 'slow', 'game-ended', '{}')",
        [ctx.contest.into(), ctx.user.into()],
    ))
    .await
    .unwrap();
    let db = ctx.app.db.clone();
    let message = ctx.message("fast");
    let mut fast =
        tokio::spawn(
            async move { notifications::publish(&db, "codelink-bracket", &message).await },
        );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut fast)
            .await
            .is_err()
    );
    txn.commit().await.unwrap();
    fast.await.unwrap().unwrap();
    let rows = notifications::after(&ctx.app.db, ctx.contest, ctx.user, "codelink-bracket", 0)
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
        ["slow", "fast"]
    );
}
