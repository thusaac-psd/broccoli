//! The queue against a real Redis: delivery, redelivery, dead letters, reply
//! routing and Redis restarts, each checked for lost messages.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::liveness::{NodeRole, TTL};
use common::worker::{Task, TaskReply, TaskResult};
use common::{DlqEnvelope, DlqErrorCode, DlqMessageType};
use mq::{CLAIM_IDLE, Delivery, MAX_DELIVERIES, Mq, ReplyOutcome, Route, Settled};
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::redis::Redis;

const BASE: &str = "ops";

async fn redis() -> (ContainerAsync<Redis>, String) {
    let container = Redis::default()
        .with_tag("7-alpine")
        .start()
        .await
        .expect("start redis");
    let port = container.get_host_port_ipv4(6379).await.expect("port");
    (container, format!("redis://127.0.0.1:{port}"))
}

/// Redis on a fixed host port, so a restarted container keeps its URL.
async fn redis_on_fixed_port() -> (ContainerAsync<Redis>, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let container = Redis::default()
        .with_tag("7-alpine")
        .with_mapped_port(port, ContainerPort::Tcp(6379))
        .with_cmd([
            "redis-server",
            "--appendonly",
            "yes",
            "--appendfsync",
            "always",
        ])
        .start()
        .await
        .expect("start redis");
    (container, format!("redis://127.0.0.1:{port}"))
}

async fn admin(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

fn task(id: &str) -> Task {
    Task {
        id: id.to_string(),
        task_type: "operation".into(),
        executor_name: "operation".into(),
        payload: serde_json::json!({}),
        reply_to: "server-1".into(),
        operation_batch_id: None,
        trace_context: None,
    }
}

fn envelope(id: &str) -> DlqEnvelope {
    DlqEnvelope {
        message_id: id.to_string(),
        message_type: DlqMessageType::OperationTask,
        submission_id: None,
        payload: serde_json::json!({ "task_id": id }),
        error_code: DlqErrorCode::MaxRetriesExceeded,
        error_message: "boom".into(),
        retry_history: vec![],
    }
}

fn ids(deliveries: &[Delivery<Task>]) -> BTreeSet<String> {
    deliveries
        .iter()
        .map(|d| d.payload.as_ref().unwrap().id.clone())
        .collect()
}

async fn set_live(conn: &mut redis::aio::MultiplexedConnection, role: NodeRole, id: &str) {
    let _: () = redis::cmd("SET")
        .arg(role.key(id))
        .arg("{}")
        .arg("PX")
        .arg(TTL.as_millis() as u64)
        .query_async(conn)
        .await
        .unwrap();
}

async fn xlen(conn: &mut redis::aio::MultiplexedConnection, key: &str) -> u64 {
    redis::cmd("XLEN").arg(key).query_async(conn).await.unwrap()
}

#[tokio::test]
async fn a_redis_older_than_7_is_refused() {
    let container = Redis::default()
        .with_tag("6.2-alpine")
        .start()
        .await
        .expect("start redis");
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let err = Mq::connect(&format!("redis://127.0.0.1:{port}"), BASE)
        .await
        .err()
        .expect("Redis 6 must be refused");
    assert!(matches!(err, mq::MqError::RedisTooOld { ref found } if found.starts_with("6.")));
}

#[tokio::test]
async fn published_tasks_are_delivered_once_and_gone_after_ack() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let consumer = mq.task_consumer("w1");
    consumer.ensure_groups().await.unwrap();
    for i in 0..3 {
        assert_eq!(
            mq.publish_task(&task(&format!("t{i}")), None)
                .await
                .unwrap(),
            Route::Shared
        );
    }

    let mut reader = consumer.reader();
    let got = reader.read(10).await.unwrap();
    assert_eq!(
        ids(&got),
        BTreeSet::from(["t0".into(), "t1".into(), "t2".into()])
    );
    assert!(got.iter().all(|d| d.deliveries == 1));
    for d in &got {
        assert_eq!(consumer.ack(&d.entry).await.unwrap(), Settled::Done);
    }

    let other = mq.task_consumer("w2");
    other.ensure_groups().await.unwrap();
    assert!(other.reader().read(10).await.unwrap().is_empty());
    let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
    assert_eq!((depths[0].unread, depths[0].pending), (0, 0));
    assert_eq!(
        xlen(&mut admin(&url).await, &mq.topology().shared()).await,
        0
    );
}

#[tokio::test]
async fn a_dead_consumers_tasks_are_redelivered_and_a_live_ones_are_not() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let dead = mq.task_consumer("dead");
    dead.ensure_groups().await.unwrap();
    mq.publish_task(&task("held"), None).await.unwrap();
    let held = dead.reader().read(1).await.unwrap();
    assert_eq!(held.len(), 1);

    // Under the default threshold the entry still belongs to its owner.
    let survivor = mq.task_consumer("survivor");
    assert!(survivor.reclaim(10).await.unwrap().is_empty());

    // Past the threshold it is taken over, with the delivery counted.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let eager = survivor.clone().with_claim_idle(Duration::from_millis(20));
    let taken = eager.reclaim(10).await.unwrap();
    assert_eq!(ids(&taken), BTreeSet::from(["held".into()]));
    assert_eq!(taken[0].deliveries, 2);

    // The old owner learns it lost the entry, and nobody else can take it now.
    assert_eq!(
        dead.refresh(&[held[0].entry.clone()]).await.unwrap().len(),
        1
    );
    assert_eq!(eager.ack(&taken[0].entry).await.unwrap(), Settled::Done);
    assert!(eager.reclaim(10).await.unwrap().is_empty());
    let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
    assert_eq!((depths[0].unread, depths[0].pending), (0, 0));
}

/// Make `entry` look idle for `idle` without waiting, keeping its owner.
async fn age(
    conn: &mut redis::aio::MultiplexedConnection,
    owner: &str,
    entry: &mq::EntryRef,
    idle: Duration,
) {
    let _: Vec<String> = redis::cmd("XCLAIM")
        .arg(&*entry.stream)
        .arg(mq::WORKERS_GROUP)
        .arg(owner)
        .arg(0)
        .arg(&entry.id)
        .arg("IDLE")
        .arg(idle.as_millis() as u64)
        .arg("JUSTID")
        .query_async(conn)
        .await
        .unwrap();
}

/// A consumer that stalled past the threshold settles nothing: the consumer
/// that took the entry over owns it.
#[tokio::test]
async fn a_stale_owner_cannot_settle_an_entry_taken_over() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let mut conn = admin(&url).await;
    let stale = mq.task_consumer("stale");
    stale.ensure_groups().await.unwrap();
    mq.publish_task(&task("contested"), None).await.unwrap();
    let mine = stale.reader().read(1).await.unwrap().remove(0);
    age(&mut conn, "stale", &mine.entry, CLAIM_IDLE * 2).await;
    let fresh = mq.task_consumer("fresh");
    let theirs = fresh.reclaim(1).await.unwrap().remove(0);

    assert_eq!(stale.ack(&mine.entry).await.unwrap(), Settled::HandedOver);
    assert_eq!(
        stale
            .dead_letter(&mine.entry, &envelope("contested"))
            .await
            .unwrap(),
        Settled::HandedOver
    );
    assert_eq!(stale.requeue(&mine).await.unwrap(), Settled::HandedOver);
    let shared = mq.topology().shared();
    let depths = mq
        .depths(&[shared.clone(), mq.topology().dead_letter()])
        .await
        .unwrap();
    assert_eq!(
        (depths[0].unread, depths[0].pending),
        (0, 1),
        "no copy added"
    );
    assert_eq!(depths[1].unread, 0, "nothing dead-lettered");

    assert_eq!(fresh.ack(&theirs.entry).await.unwrap(), Settled::Done);
    assert_eq!(xlen(&mut conn, &shared).await, 0);
}

#[tokio::test]
async fn refreshing_an_entry_keeps_it_from_being_reclaimed() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let owner = mq.task_consumer("owner");
    owner.ensure_groups().await.unwrap();
    mq.publish_task(&task("long-job"), None).await.unwrap();
    let held = owner.reader().read(1).await.unwrap();

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        owner
            .refresh(&[held[0].entry.clone()])
            .await
            .unwrap()
            .is_empty()
    );
    let other = mq
        .task_consumer("other")
        .with_claim_idle(Duration::from_millis(300));
    assert!(
        other.reclaim(10).await.unwrap().is_empty(),
        "an entry refreshed just now is not idle"
    );
}

#[tokio::test]
async fn poison_and_undecodable_entries_end_in_the_dead_letter_stream() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let mut conn = admin(&url).await;
    let shared = mq.topology().shared();

    let first = mq.task_consumer("c0");
    first.ensure_groups().await.unwrap();
    mq.publish_task(&task("poison"), None).await.unwrap();
    let mut last = first.reader().read(1).await.unwrap().remove(0);
    let mut owner = first.clone();
    // Each consumer takes it and dies holding it.
    for n in 1..=MAX_DELIVERIES {
        age(
            &mut conn,
            &format!("c{}", n - 1),
            &last.entry,
            CLAIM_IDLE * 2,
        )
        .await;
        owner = mq.task_consumer(&format!("c{n}"));
        last = owner.reclaim(1).await.unwrap().remove(0);
    }
    assert_eq!(last.deliveries, MAX_DELIVERIES + 1);
    assert!(last.is_poison());
    assert_eq!(
        owner
            .dead_letter(&last.entry, &envelope("poison"))
            .await
            .unwrap(),
        Settled::Done
    );

    let _: String = redis::cmd("XADD")
        .arg(&shared)
        .arg("*")
        .arg("p")
        .arg("{not json")
        .query_async(&mut conn)
        .await
        .unwrap();
    let bad = first.reader().read(1).await.unwrap().remove(0);
    assert!(bad.payload.is_err());
    assert_eq!(bad.raw_payload(), "{not json");
    assert_eq!(
        first
            .dead_letter(&bad.entry, &envelope("bad"))
            .await
            .unwrap(),
        Settled::Done
    );

    assert_eq!(
        xlen(&mut conn, &shared).await,
        0,
        "nothing stays in the work stream"
    );
    let dlq = mq.dead_letter_consumer("server-1");
    dlq.ensure_groups().await.unwrap();
    let letters = dlq.reader().read(10).await.unwrap();
    let got: BTreeSet<String> = letters
        .iter()
        .map(|d| d.payload.as_ref().unwrap().message_id.clone())
        .collect();
    assert_eq!(got, BTreeSet::from(["poison".into(), "bad".into()]));
}

#[tokio::test]
async fn replies_reach_only_their_server_and_a_gone_server_drops_them() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let mut conn = admin(&url).await;
    for server in ["s1", "s2"] {
        let _: String = redis::cmd("XADD")
            .arg(mq.topology().reply(server))
            .arg("MAXLEN")
            .arg(0)
            .arg("*")
            .arg("_")
            .arg("")
            .query_async(&mut conn)
            .await
            .unwrap();
    }
    let mut s1 = mq.reply_inbox("s1");
    let mut s2 = mq.reply_inbox("s2");

    let reply = |id: &str| TaskReply::Completed {
        result: TaskResult {
            task_id: id.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
        },
    };
    for i in 0..3 {
        assert_eq!(
            mq.publish_reply("s1", &reply(&format!("r{i}")))
                .await
                .unwrap(),
            ReplyOutcome::Delivered
        );
    }
    let got: Vec<String> = s1
        .next_batch()
        .await
        .unwrap()
        .into_iter()
        .map(|r| match r.unwrap() {
            TaskReply::Completed { result } => result.task_id,
            TaskReply::Started { notification } => notification.task_id,
        })
        .collect();
    assert_eq!(got, ["r0", "r1", "r2"], "in publish order");
    assert!(s2.next_batch().await.unwrap().is_empty(), "s2 gets nothing");

    // The next read deletes what s1 has read.
    assert!(s1.next_batch().await.unwrap().is_empty());
    assert_eq!(xlen(&mut conn, &mq.topology().reply("s1")).await, 0);

    // Without its reply stream a server gets nothing, live or not.
    assert_eq!(
        mq.publish_reply("gone", &reply("late")).await.unwrap(),
        ReplyOutcome::ServerGone
    );
    let exists: bool = redis::cmd("EXISTS")
        .arg(mq.topology().reply("gone"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(!exists, "a reply never creates a stream");
}

fn completed(id: &str) -> TaskReply {
    TaskReply::Completed {
        result: TaskResult {
            task_id: id.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
        },
    }
}

fn task_ids(batch: Vec<Result<TaskReply, mq::DecodeError>>) -> Vec<String> {
    batch
        .into_iter()
        .map(|r| match r.unwrap() {
            TaskReply::Completed { result } => result.task_id,
            TaskReply::Started { notification } => notification.task_id,
        })
        .collect()
}

/// Only replies already returned are deleted, and a stream recreated with
/// ids behind the cursor (a Redis clock behind the old one) is read from the
/// start.
#[tokio::test]
async fn the_reply_inbox_deletes_what_it_read_and_survives_a_recreated_stream() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let mut conn = admin(&url).await;
    let key = mq.topology().reply("s1");
    let add = |id: &'static str, reply: TaskReply| {
        let key = key.clone();
        let url = url.clone();
        async move {
            let _: String = redis::cmd("XADD")
                .arg(&key)
                .arg(id)
                .arg("p")
                .arg(serde_json::to_string(&reply).unwrap())
                .query_async(&mut admin(&url).await)
                .await
                .unwrap();
        }
    };
    let mut inbox = mq.reply_inbox("s1");
    add("5000-0", completed("r0")).await;
    assert_eq!(task_ids(inbox.next_batch().await.unwrap()), ["r0"]);
    add("5000-1", completed("r1")).await;
    assert_eq!(task_ids(inbox.next_batch().await.unwrap()), ["r1"]);
    assert_eq!(
        xlen(&mut conn, &key).await,
        1,
        "r0 is deleted; r1 waits for the next read"
    );

    // The stream is lost and recreated with lower ids.
    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap();
    add("10-0", completed("after-recreate")).await;
    let mut got = Vec::new();
    while got.is_empty() {
        got = task_ids(inbox.next_batch().await.unwrap());
    }
    assert_eq!(got, ["after-recreate"]);
    assert_eq!(
        xlen(&mut conn, &key).await,
        1,
        "a recreated reply is never trimmed unread"
    );
}

#[tokio::test]
async fn pinned_tasks_follow_their_worker_and_move_to_the_shared_stream_when_it_dies() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let mut conn = admin(&url).await;
    set_live(&mut conn, NodeRole::Worker, "w1").await;
    let w1 = mq.task_consumer("w1");
    w1.ensure_groups().await.unwrap();

    assert_eq!(
        mq.publish_task(&task("p0"), Some("w1")).await.unwrap(),
        Route::Pinned
    );
    assert_eq!(
        mq.publish_task(&task("p1"), Some("w1")).await.unwrap(),
        Route::Pinned
    );
    assert_eq!(
        mq.publish_task(&task("to-dead"), Some("nobody"))
            .await
            .unwrap(),
        Route::Shared,
        "a dead worker gets no pinned task"
    );
    // w1 takes one entry of each stream (COUNT applies per stream) and dies.
    let taken = w1.reader().read(1).await.unwrap();
    assert_eq!(ids(&taken), BTreeSet::from(["p0".into(), "to-dead".into()]));
    assert_eq!(
        mq.drain_dead_private_streams().await.unwrap(),
        0,
        "w1 is live"
    );
    let _: () = redis::cmd("DEL")
        .arg(NodeRole::Worker.key("w1"))
        .query_async(&mut conn)
        .await
        .unwrap();

    assert_eq!(mq.drain_dead_private_streams().await.unwrap(), 2);
    let w2 = mq.task_consumer("w2");
    w2.ensure_groups().await.unwrap();
    let mut got = w2.reader().read(10).await.unwrap();
    got.sort_by_key(|d| d.payload.as_ref().unwrap().id.clone());
    let summary: Vec<(String, u32)> = got
        .iter()
        .map(|d| (d.payload.as_ref().unwrap().id.clone(), d.deliveries))
        .collect();
    assert_eq!(
        summary,
        [("p0".into(), 2), ("p1".into(), 1)],
        "the taken task keeps its delivery; w1's shared entry waits for a reclaim"
    );
    let pinned: Vec<String> = redis::cmd("SMEMBERS")
        .arg(mq.topology().pinned_set())
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(pinned.is_empty());
    assert_eq!(xlen(&mut conn, &mq.topology().private("w1")).await, 0);
}

#[tokio::test]
async fn requeued_entries_go_back_to_their_stream() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let w1 = mq.task_consumer("w1");
    w1.ensure_groups().await.unwrap();
    mq.publish_task(&task("unstarted"), None).await.unwrap();
    let d = w1.reader().read(1).await.unwrap().remove(0);
    assert_eq!(w1.requeue(&d).await.unwrap(), Settled::Done);

    let w2 = mq.task_consumer("w2");
    w2.ensure_groups().await.unwrap();
    let again = w2.reader().read(1).await.unwrap();
    assert_eq!(ids(&again), BTreeSet::from(["unstarted".into()]));
    assert_eq!(
        again[0].deliveries, 1,
        "a task never started is not counted"
    );
    let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
    assert_eq!(depths[0].pending, 1, "only the new delivery is pending");
}

/// Every command fails fast while Redis is down instead of hanging, and the
/// queue works again once it is back. Work in flight across the restart is
/// neither lost nor duplicated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redis_restart_mid_flight_loses_nothing() {
    let (container, url) = redis_on_fixed_port().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let w1 = mq.task_consumer("w1");
    w1.ensure_groups().await.unwrap();
    for i in 0..4 {
        mq.publish_task(&task(&format!("t{i}")), None)
            .await
            .unwrap();
    }
    let mut reader = w1.reader();
    let in_flight = reader.read(2).await.unwrap();
    assert_eq!(in_flight.len(), 2);

    container.stop().await.expect("stop redis");
    let started = Instant::now();
    assert!(mq.publish_task(&task("during"), None).await.is_err());
    assert!(reader.read(1).await.is_err());
    assert!(w1.ack(&in_flight[0].entry).await.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "errors surface within the command deadlines, took {:?}",
        started.elapsed()
    );

    container.start().await.expect("restart redis");
    let deadline = Instant::now() + Duration::from_secs(30);
    while mq.publish_task(&task("t4"), None).await.is_err() {
        assert!(Instant::now() < deadline, "the queue never recovered");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The two in-flight entries are still ours; the rest are still unread.
    let mut seen = BTreeSet::new();
    for d in &in_flight {
        assert_eq!(w1.ack(&d.entry).await.unwrap(), Settled::Done);
        seen.insert(d.payload.as_ref().unwrap().id.clone());
    }
    loop {
        let batch = reader.read(10).await.unwrap();
        if batch.is_empty() {
            break;
        }
        for d in &batch {
            assert!(
                seen.insert(d.payload.as_ref().unwrap().id.clone()),
                "no duplicates"
            );
            assert_eq!(w1.ack(&d.entry).await.unwrap(), Settled::Done);
        }
    }
    let expected: BTreeSet<String> = (0..5).map(|i| format!("t{i}")).collect();
    assert_eq!(seen, expected);
}

/// A restart that lost the data (and so the consumer groups) is recovered by
/// recreating the groups on the next read, without surfacing an error.
#[tokio::test]
async fn a_lost_consumer_group_is_recreated() {
    let (_c, url) = redis().await;
    let mq = Mq::connect(&url, BASE).await.unwrap();
    let w1 = mq.task_consumer("w1");
    w1.ensure_groups().await.unwrap();
    let _: () = redis::cmd("FLUSHALL")
        .query_async(&mut admin(&url).await)
        .await
        .unwrap();

    let mut reader = w1.reader();
    assert!(reader.read(1).await.unwrap().is_empty());
    assert!(w1.reclaim(1).await.unwrap().is_empty());
    mq.publish_task(&task("after-flush"), None).await.unwrap();
    assert_eq!(
        ids(&reader.read(1).await.unwrap()),
        BTreeSet::from(["after-flush".into()])
    );
}
