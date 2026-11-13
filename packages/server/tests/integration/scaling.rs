//! Replies across server replicas, through the real queue.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use broccoli_server_sdk::types::OperationTask;
use common::liveness::NodeRole;
use common::node_identity::{NodeClaim, NodeHeartbeat};
use common::storage::filesystem::FilesystemBlobStore;
use common::worker::{TaskReply, TaskResult};
use dashmap::DashMap;
use mq::Mq;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use server::consumers::consume_operation_results;
use server::host_funcs::context::OperationHostDeps;
use server::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry;
use server::registry::{OperationWaiter, OperationWaiters};
use server::services::operation_batch::{
    OperationTaskPublisher, next_operation_result, start_operation_batch,
};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::oneshot;
use tokio::time::timeout;

async fn redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let redis = Redis::default()
        .with_tag("7-alpine")
        .start()
        .await
        .expect("failed to start Redis container");
    let port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("failed to get Redis port");
    (redis, format!("redis://127.0.0.1:{port}"))
}

/// Server `id` as it runs: its liveness key, owning its reply stream.
async fn live_server(url: &str, mq: &Mq, id: &str) -> NodeHeartbeat {
    NodeClaim::claim_at_boot(
        redis::Client::open(url).unwrap(),
        mq.topology().namespace(),
        NodeRole::Server,
        id,
        serde_json::json!({}),
    )
    .await
    .expect("claim the server id")
    .own_stream(mq.topology().reply(id), mq::REPLY_STREAM_TTL, || {})
    .spawn_heartbeat(|| serde_json::json!({}))
}

/// Wait for the heartbeat's first renewal to create `id`'s reply stream.
async fn reply_stream_ready(url: &str, mq: &Mq, id: &str) {
    let mut conn = redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    for _ in 0..100 {
        let exists: bool = redis::cmd("EXISTS")
            .arg(mq.topology().reply(id))
            .query_async(&mut conn)
            .await
            .unwrap();
        if exists {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the reply stream of {id} was never created");
}

fn completed(task_id: &str) -> TaskReply {
    TaskReply::Completed {
        result: TaskResult {
            task_id: task_id.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: Some("operation".to_string()),
            operation: Some("operation".to_string()),
            worker_id: Some("worker-a".to_string()),
        },
    }
}

#[tokio::test]
async fn a_reply_reaches_only_the_replica_that_published_the_task() {
    let (_redis, url) = redis().await;
    let mq = Mq::connect(&url, "ops").await.expect("mq");
    let _a = live_server(&url, &mq, "replica-a").await;
    let _b = live_server(&url, &mq, "replica-b").await;
    reply_stream_ready(&url, &mq, "replica-a").await;
    reply_stream_ready(&url, &mq, "replica-b").await;
    let (metrics, _registry, _provider) = init_local_metrics("broccoli-scaling-test");

    let waiters_a: OperationWaiters = Arc::new(DashMap::new());
    let waiters_b: OperationWaiters = Arc::new(DashMap::new());
    let (tx_a, rx_a) = oneshot::channel();
    let (tx_b, rx_b) = oneshot::channel();
    waiters_a.insert("task-1".to_string(), OperationWaiter::new(tx_a));
    waiters_b.insert("task-1".to_string(), OperationWaiter::new(tx_b));
    let consumer_a = tokio::spawn(consume_operation_results(
        mq.reply_inbox("replica-a"),
        Arc::clone(&waiters_a),
        EvaluateBatchOpsRegistry::default(),
        metrics.clone(),
    ));
    let consumer_b = tokio::spawn(consume_operation_results(
        mq.reply_inbox("replica-b"),
        Arc::clone(&waiters_b),
        EvaluateBatchOpsRegistry::default(),
        metrics,
    ));

    assert_eq!(
        mq.publish_reply("replica-a", &completed("task-1"))
            .await
            .unwrap(),
        mq::ReplyOutcome::Delivered
    );
    let delivered = timeout(Duration::from_secs(5), rx_a)
        .await
        .expect("replica A did not receive its result")
        .expect("replica A waiter dropped");
    assert_eq!(delivered.task_id, "task-1");
    assert!(
        timeout(Duration::from_millis(500), rx_b).await.is_err(),
        "replica B must not receive replica A's result"
    );
    assert!(waiters_b.contains_key("task-1"));

    consumer_a.abort();
    consumer_b.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operation_result_burst_has_no_missing_waiters() {
    const OP_COUNT: usize = 1_000;

    let (_redis, url) = redis().await;
    let mq = Mq::connect(&url, "ops").await.expect("mq");
    let server_id = "burst-server";
    let _server = live_server(&url, &mq, server_id).await;
    reply_stream_ready(&url, &mq, server_id).await;
    let (metrics, registry, _metrics_provider) = init_local_metrics("broccoli-waiter-burst-test");
    let operation_batches = Arc::new(DashMap::new());
    let operation_waiters = Arc::new(DashMap::new());
    let evaluate_ops_registry = EvaluateBatchOpsRegistry::default();

    let consumer = tokio::spawn(consume_operation_results(
        mq.reply_inbox(server_id),
        Arc::clone(&operation_waiters),
        evaluate_ops_registry.clone(),
        metrics.clone(),
    ));

    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let deps = OperationHostDeps {
        plugin_manager: None,
        operation_publisher: Some(Arc::new(ImmediateResultPublisher { mq: mq.clone() })),
        operation_batches: Arc::clone(&operation_batches),
        operation_waiters: Arc::clone(&operation_waiters),
        server_id: server_id.to_string(),
        blob_store: Arc::new(
            FilesystemBlobStore::new(tmp.path().join("blobs"), 16 * 1024 * 1024)
                .await
                .expect("blob store should initialize"),
        ),
        metrics: Some(metrics.clone()),
        evaluate_ops_registry,
        operation_batch_publish_concurrency: 128,
    };

    let batch_id = start_operation_batch(
        "waiter-burst-test".to_string(),
        deps.clone(),
        (0..OP_COUNT).map(|_| empty_operation()).collect(),
    )
    .await
    .expect("operation batch should start");

    let mut delivered = 0usize;
    while delivered < OP_COUNT {
        let result = next_operation_result(
            "waiter-burst-test",
            &operation_batches,
            Some(&metrics),
            &batch_id,
            Duration::from_secs(10),
        )
        .expect("next operation result should not fail")
        .expect("operation result should arrive before timeout");
        assert!(result.success, "operation result should be successful");
        delivered += 1;
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    let metrics_text = prometheus_text(&registry);
    assert_eq!(
        prometheus_counter_sum_with_label(
            &metrics_text,
            "broccoli_operation_result_messages_total",
            "outcome=\"no_waiter\"",
        ),
        0.0,
        "operation result consumer observed missing waiters under immediate 1000-op reply burst"
    );
    assert_eq!(
        prometheus_counter_sum_with_label(
            &metrics_text,
            "broccoli_operation_result_messages_total",
            "outcome=\"delivered\"",
        ),
        OP_COUNT as f64,
        "all completed replies should be delivered to waiters"
    );
    assert!(
        operation_waiters.is_empty(),
        "waiter map should be empty after draining the burst"
    );

    consumer.abort();
}

/// Stands in for a worker that answers each task the moment it is published.
struct ImmediateResultPublisher {
    mq: Mq,
}

#[async_trait]
impl OperationTaskPublisher for ImmediateResultPublisher {
    async fn publish_operation_task(
        &self,
        task: &common::worker::Task,
        _pin: Option<&str>,
    ) -> anyhow::Result<mq::Route> {
        let outcome = self
            .mq
            .publish_reply(&task.reply_to, &completed(&task.id))
            .await?;
        anyhow::ensure!(outcome == mq::ReplyOutcome::Delivered, "{outcome:?}");
        Ok(mq::Route::Shared)
    }
}

fn empty_operation() -> OperationTask {
    OperationTask {
        environments: Vec::new(),
        tasks: Vec::new(),
        channels: Vec::new(),
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    }
}

fn prometheus_text(registry: &prometheus::Registry) -> String {
    use prometheus::Encoder;

    let encoder = prometheus::TextEncoder::new();
    let families = registry.gather();
    let mut buf = Vec::new();
    encoder
        .encode(&families, &mut buf)
        .expect("metrics should encode");
    String::from_utf8(buf).expect("metrics should be UTF-8")
}

fn init_local_metrics(
    service_name: &str,
) -> (
    common::metrics::Metrics,
    prometheus::Registry,
    SdkMeterProvider,
) {
    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .build()
        .expect("metrics exporter should build");
    let provider = SdkMeterProvider::builder().with_reader(exporter).build();
    let scope = opentelemetry::InstrumentationScope::builder(service_name.to_string()).build();
    let metrics = common::metrics::Metrics::new(&provider.meter_with_scope(scope));

    (metrics, registry, provider)
}

fn prometheus_counter_sum_with_label(metrics_text: &str, metric_name: &str, label: &str) -> f64 {
    metrics_text
        .lines()
        .filter(|line| line.starts_with(metric_name) && line.contains(label))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, value)| value.parse::<f64>().ok())
        .sum()
}
