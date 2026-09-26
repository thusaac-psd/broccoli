use broccoli_server_sdk::types::TestCaseVerdict;
use common::worker::TaskResult;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, oneshot};

#[derive(Clone, Debug)]
pub struct PluginHandler {
    pub plugin_id: String,
    pub function_name: String,
}

#[derive(Clone, Debug)]
pub struct ContestTypeHandlers {
    pub plugin_id: String,
    pub submission_fn: String,
    pub code_run_fn: String,
}

pub struct BatchState<T> {
    pub result_rx: flume::Receiver<T>,
    pub pending_count: Arc<std::sync::atomic::AtomicUsize>,
    pub created_at: Instant,
    pub cleanup_keys: Arc<Vec<String>>,
    pub poisoned: AtomicBool,
}

pub type ContestTypeRegistry = Arc<RwLock<HashMap<String, ContestTypeHandlers>>>;

pub type EvaluatorRegistry = Arc<RwLock<HashMap<String, PluginHandler>>>;

/// The resolve + interpret functions a checker plugin registers for a format
/// under checker fusion. `resolve_fn` returns a `CheckerStage` to splice into the
/// run op; `interpret_fn` turns the check step's small result into a
/// `CheckerVerdict`. Bundled (like [`ContestTypeHandlers`]) so a format registers
/// once and both host fns share one lookup.
#[derive(Clone, Debug)]
pub struct CheckerStageHandlers {
    pub plugin_id: String,
    pub resolve_fn: String,
    pub interpret_fn: String,
}

pub type CheckerStageRegistry = Arc<RwLock<HashMap<String, CheckerStageHandlers>>>;

#[derive(Clone, Debug)]
pub struct LanguageResolverEntry {
    pub plugin_id: String,
    pub function_name: String,
    pub display_name: String,
    pub default_filename: String,
    pub extensions: Vec<String>,
    pub template: String,
}

pub type LanguageResolverRegistry = Arc<RwLock<HashMap<String, LanguageResolverEntry>>>;

pub type OperationBatches = Arc<DashMap<String, BatchState<TaskResult>>>;

pub type EvaluateBatches = Arc<DashMap<String, BatchState<TestCaseVerdict>>>;

pub fn spawn_batch_reaper<T: Send + Sync + 'static, F>(
    label: &'static str,
    batches: Arc<DashMap<String, BatchState<T>>>,
    max_age: Duration,
    metrics: Option<common::metrics::Metrics>,
    on_expire: F,
) where
    F: Fn(&str, &BatchState<T>) + Send + Sync + 'static,
{
    let on_expire = Arc::new(on_expire);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut poisoned_count = 0u32;
            let mut reaped_count = 0u32;
            let on_expire = on_expire.clone();
            batches.retain(|batch_id, state| {
                if state.created_at.elapsed() <= max_age {
                    return true;
                }
                if state.poisoned.load(Ordering::Relaxed) {
                    reaped_count += 1;
                    if let Some(metrics) = metrics.as_ref() {
                        let attrs = [
                            opentelemetry::KeyValue::new("batch.kind", label),
                            opentelemetry::KeyValue::new("phase", "reaped"),
                        ];
                        metrics.batch_reaped_total.add(1, &attrs);
                        metrics
                            .batch_active
                            .add(-1, &[opentelemetry::KeyValue::new("batch.kind", label)]);
                    }
                    false
                } else {
                    on_expire(batch_id, state);
                    state.poisoned.store(true, Ordering::Relaxed);
                    poisoned_count += 1;
                    if let Some(metrics) = metrics.as_ref() {
                        metrics.batch_reaped_total.add(
                            1,
                            &[
                                opentelemetry::KeyValue::new("batch.kind", label),
                                opentelemetry::KeyValue::new("phase", "poisoned"),
                            ],
                        );
                    }
                    true
                }
            });
            if poisoned_count > 0 || reaped_count > 0 {
                tracing::warn!(poisoned_count, reaped_count, label, "Batch reaper cycle");
            }
        }
    });
}

#[derive(Clone)]
pub struct OperationWaiter {
    pub result_tx: Arc<std::sync::Mutex<Option<oneshot::Sender<TaskResult>>>>,
}

impl OperationWaiter {
    pub fn new(result_tx: oneshot::Sender<TaskResult>) -> Self {
        Self {
            result_tx: Arc::new(std::sync::Mutex::new(Some(result_tx))),
        }
    }
}

pub type OperationWaiters = Arc<DashMap<String, OperationWaiter>>;
