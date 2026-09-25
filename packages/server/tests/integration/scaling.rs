use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use broccoli_server_sdk::types::OperationTask;
use chrono::{TimeDelta, Utc};
use common::storage::filesystem::FilesystemBlobStore;
use common::worker::TaskResult;
use dashmap::DashMap;
use mq::{MqConfig, MqQueue, config::PublishConfig, init_mq};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use redis::AsyncCommands;
use server::config::per_replica_result_queue_name;
use server::consumers::consume_operation_results;
use server::dispatcher::sweeper::sweep_once;
use server::host_funcs::context::OperationHostDeps;
use server::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry;
use server::registry::{OperationWaiter, OperationWaiters};
use server::services::operation_batch::{
    OperationTaskPublisher, next_operation_result, start_operation_batch,
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test]
async fn per_replica_result_queue_delivers_to_originating_replica_only() {
    let redis = Redis::default()
        .start()
        .await
        .expect("failed to start Redis container");
    let port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("failed to get Redis port");
    let redis_url = format!("redis://127.0.0.1:{port}");

    let mq_a = Arc::new(
        init_mq(MqConfig {
            url: redis_url.clone(),
            pool_size: 2,
        })
        .await
        .expect("failed to create replica A MQ client"),
    );
    let mq_b = Arc::new(
        init_mq(MqConfig {
            url: redis_url.clone(),
            pool_size: 2,
        })
        .await
        .expect("failed to create replica B MQ client"),
    );
    let publisher = init_mq(MqConfig {
        url: redis_url,
        pool_size: 2,
    })
    .await
    .expect("failed to create publisher MQ client");

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let base_queue = format!("operation_results.scaling_{suffix}");
    let queue_a = per_replica_result_queue_name(&base_queue, "replica-a");
    let queue_b = per_replica_result_queue_name(&base_queue, "replica-b");
    let (metrics, _registry, _metrics_provider) = init_local_metrics("broccoli-scaling-test");

    let waiters_a: OperationWaiters = Arc::new(DashMap::new());
    let waiters_b: OperationWaiters = Arc::new(DashMap::new());
    let evaluate_ops_registry = EvaluateBatchOpsRegistry::default();
    let (tx_a, rx_a) = oneshot::channel();
    let (tx_b, rx_b) = oneshot::channel();
    waiters_a.insert("task-1".to_string(), OperationWaiter::new(tx_a));
    waiters_b.insert("task-1".to_string(), OperationWaiter::new(tx_b));

    let consumer_a = tokio::spawn(consume_operation_results(
        Arc::clone(&mq_a),
        Arc::clone(&waiters_a),
        evaluate_ops_registry.clone(),
        queue_a.clone(),
        metrics.clone(),
        std::time::Duration::from_millis(20),
    ));
    let consumer_b = tokio::spawn(consume_operation_results(
        Arc::clone(&mq_b),
        Arc::clone(&waiters_b),
        evaluate_ops_registry,
        queue_b,
        metrics,
        std::time::Duration::from_millis(20),
    ));

    tokio::time::sleep(Duration::from_millis(250)).await;

    publisher
        .publish(
            &queue_a,
            None,
            &TaskResult {
                task_id: "task-1".to_string(),
                success: true,
                output: serde_json::json!({ "replica": "a" }),
                error: None,
                task_type: Some("operation".to_string()),
                operation: Some("operation".to_string()),
                worker_id: Some("worker-a".to_string()),
                enqueued_at_unix_ms: Some(1_234),
            },
            None,
        )
        .await
        .expect("failed to publish task result");

    let delivered = timeout(Duration::from_secs(5), rx_a)
        .await
        .expect("replica A did not receive its result")
        .expect("replica A waiter dropped");
    assert_eq!(delivered.task_id, "task-1");
    assert!(delivered.success);

    assert!(
        timeout(Duration::from_millis(500), rx_b).await.is_err(),
        "replica B must not receive replica A's result"
    );
    assert!(waiters_b.contains_key("task-1"));

    consumer_a.abort();
    consumer_b.abort();
}

#[tokio::test]
async fn ghost_reply_queue_lifecycle_debounces_then_deletes_family() {
    let redis = Redis::default()
        .start()
        .await
        .expect("failed to start Redis container");
    let port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("failed to get Redis port");
    let redis_url = format!("redis://127.0.0.1:{port}");

    let publisher = init_mq(MqConfig {
        url: redis_url.clone(),
        pool_size: 2,
    })
    .await
    .expect("failed to create publisher MQ client");
    let redis_client = redis::Client::open(redis_url).expect("redis URL should be valid");
    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("failed to connect to Redis");

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let base_queue = format!("operation_results.ghost_{suffix}");
    let old_server_id = format!("api-old-{suffix}");
    let new_server_id = format!("api-new-{suffix}");
    let reply_queue = per_replica_result_queue_name(&base_queue, &old_server_id);
    let dead_since_key = format!("broccoli:server:dead_since:{old_server_id}");
    let old_heartbeat_key = format!("broccoli:server:heartbeat:{old_server_id}");
    let new_heartbeat_key = format!("broccoli:server:heartbeat:{new_server_id}");

    let _: () = conn
        .set_ex(&old_heartbeat_key, "alive", 60)
        .await
        .expect("old heartbeat should be written");
    let _: () = conn
        .set_ex(&new_heartbeat_key, "alive", 60)
        .await
        .expect("new heartbeat should be written");

    publisher
        .publish(
            &reply_queue,
            None,
            &TaskResult {
                task_id: "ghost-task".to_string(),
                success: true,
                output: serde_json::json!({ "reply": true }),
                error: None,
                task_type: Some("operation".to_string()),
                operation: Some("operation".to_string()),
                worker_id: Some("worker-ghost".to_string()),
                enqueued_at_unix_ms: Some(1_234),
            },
            None,
        )
        .await
        .expect("reply queue should be populated through real MQ publish");
    let processing_key = format!("{reply_queue}_processing");
    let failed_key = format!("{reply_queue}_failed");
    let fairness_key = format!("{reply_queue}_fairness_set");
    let _: () = conn
        .lpush(&processing_key, "in-flight")
        .await
        .expect("processing sibling should be written");
    let _: () = conn
        .lpush(&failed_key, "failed")
        .await
        .expect("failed sibling should be written");
    let _: () = conn
        .sadd(&fairness_key, "contestant-1")
        .await
        .expect("fairness sibling should be written");

    assert!(redis_exists(&mut conn, &reply_queue).await);
    sweep_once(&redis_client, &base_queue, false)
        .await
        .expect("sweep with live owner should succeed");
    assert!(redis_exists(&mut conn, &reply_queue).await);
    assert!(!redis_exists(&mut conn, &dead_since_key).await);

    let _: () = conn
        .del(&old_heartbeat_key)
        .await
        .expect("old heartbeat should be removed");
    sweep_once(&redis_client, &base_queue, false)
        .await
        .expect("first dead-owner sweep should succeed");
    assert!(redis_exists(&mut conn, &reply_queue).await);
    assert!(redis_exists(&mut conn, &dead_since_key).await);

    let stale_dead_since = (Utc::now() - TimeDelta::seconds(3_601)).to_rfc3339();
    let _: () = conn
        .set(&dead_since_key, stale_dead_since)
        .await
        .expect("dead_since should be rewound past debounce window");
    sweep_once(&redis_client, &base_queue, false)
        .await
        .expect("stale ghost sweep should succeed");

    for key in [
        reply_queue.as_str(),
        processing_key.as_str(),
        failed_key.as_str(),
        fairness_key.as_str(),
        dead_since_key.as_str(),
    ] {
        assert!(
            !redis_exists(&mut conn, key).await,
            "{key} should be deleted"
        );
    }
    assert!(
        redis_exists(&mut conn, &new_heartbeat_key).await,
        "new API replica heartbeat must not be treated as part of old queue family"
    );
}

async fn redis_exists(conn: &mut redis::aio::MultiplexedConnection, key: &str) -> bool {
    redis::cmd("EXISTS")
        .arg(key)
        .query_async::<bool>(conn)
        .await
        .expect("EXISTS should succeed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operation_result_burst_has_no_missing_waiters() {
    const OP_COUNT: usize = 1_000;

    let redis = Redis::default()
        .start()
        .await
        .expect("failed to start Redis container");
    let port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("failed to get Redis port");
    let redis_url = format!("redis://127.0.0.1:{port}");
    let mq = Arc::new(
        init_mq(MqConfig {
            url: redis_url,
            pool_size: 4,
        })
        .await
        .expect("failed to create MQ client"),
    );

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let result_queue = format!("operation_results.waiter_burst_{suffix}");
    let (metrics, registry, _metrics_provider) = init_local_metrics("broccoli-waiter-burst-test");
    let operation_batches = Arc::new(DashMap::new());
    let operation_waiters = Arc::new(DashMap::new());
    let evaluate_ops_registry = EvaluateBatchOpsRegistry::default();

    let consumer = tokio::spawn(consume_operation_results(
        Arc::clone(&mq),
        Arc::clone(&operation_waiters),
        evaluate_ops_registry.clone(),
        result_queue.clone(),
        metrics.clone(),
        std::time::Duration::from_millis(20),
    ));
    tokio::time::sleep(Duration::from_millis(250)).await;

    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let deps = OperationHostDeps {
        plugin_manager: None,
        operation_publisher: Some(Arc::new(ImmediateResultPublisher {
            mq: Arc::clone(&mq),
            result_queue: result_queue.clone(),
        })),
        operation_batches: Arc::clone(&operation_batches),
        operation_waiters: Arc::clone(&operation_waiters),
        operation_queue_name: format!("operation_tasks.waiter_burst_{suffix}"),
        operation_result_queue_name: result_queue,
        blob_store: Arc::new(
            FilesystemBlobStore::new(tmp.path().join("blobs"), 16 * 1024 * 1024)
                .await
                .expect("blob store should initialize"),
        ),
        metrics: Some(metrics.clone()),
        evaluate_ops_registry,
        operation_batch_publish_concurrency: 128,
        redis_client: None,
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

struct ImmediateResultPublisher {
    mq: Arc<MqQueue>,
    result_queue: String,
}

#[async_trait]
impl OperationTaskPublisher for ImmediateResultPublisher {
    async fn publish_operation_task(
        &self,
        _target_queue: &str,
        task: &common::worker::Task,
        _publish_config: Option<PublishConfig>,
    ) -> anyhow::Result<()> {
        let result = TaskResult {
            task_id: task.id.clone(),
            success: true,
            output: serde_json::json!({ "immediate": true }),
            error: None,
            task_type: Some(task.task_type.clone()),
            operation: Some(task.executor_name.clone()),
            worker_id: Some("waiter-burst-worker".to_string()),
            enqueued_at_unix_ms: task.enqueued_at_unix_ms,
        };
        self.mq
            .publish(&self.result_queue, None, &result, None)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("failed to publish immediate operation result: {e}"))
    }
}

fn empty_operation() -> OperationTask {
    OperationTask {
        environments: Vec::new(),
        tasks: Vec::new(),
        channels: Vec::new(),
        priority: None,
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
