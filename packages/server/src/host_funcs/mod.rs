pub mod checker;
pub mod config;
pub mod context;
pub mod dispatch;
pub mod evaluate;
pub mod evaluate_ops_registry;
pub mod language;
pub mod logger;
pub mod registry;
pub mod sql;
pub mod storage;
pub mod submissions;
pub mod timer;

use crate::host_funcs::context::HostFunctionDeps;
use common::metrics::Metrics;
use extism::{Function, UserData, ValType};
use opentelemetry::KeyValue;
use plugin_core::host::HostFunctionRegistry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Semaphore;
use tracing::Span;

/// Acquire a `std::sync::Mutex` guard, mapping a poison error to an Extism host
/// error. Deduplicates the
/// `.lock().map_err(|_| extism::Error::msg("Lock poisoned"))?` idiom repeated
/// across the raw host functions.
pub(crate) fn lock_or_poison<T>(
    m: &StdMutex<T>,
) -> Result<std::sync::MutexGuard<'_, T>, extism::Error> {
    m.lock().map_err(|_| extism::Error::msg("Lock poisoned"))
}

// regression guard, see UP#14g
//
// Available for any future host_fn that re-introduces a `block_in_place`
// wrapper. Not called anywhere in shipping code - a non-zero count of
// `broccoli.host_fn.block_in_place` indicates someone reverted UP#13's
// collapse and CI should fail the build via UP#14g.
#[track_caller]
#[allow(dead_code)]
pub(crate) fn record_block_in_place_regression(metrics: &Option<Metrics>, host_fn: &str) {
    if let Some(m) = metrics {
        let loc = std::panic::Location::caller();
        m.host_fn_block_in_place_total.add(
            1,
            &[
                KeyValue::new("host_fn", host_fn.to_string()),
                KeyValue::new("file", loc.file().to_string()),
                KeyValue::new("line", loc.line() as i64),
            ],
        );
    }
}

pub(crate) fn host_fn_span(host_fn: &'static str, plugin_id: &str) -> Span {
    tracing::info_span!(
        parent: &Span::current(),
        "host_fn",
        host_fn,
        plugin_id
    )
}

pub fn init_host_functions(deps: HostFunctionDeps) -> HostFunctionRegistry {
    let mut hr = HostFunctionRegistry::new();
    // `broccoli_plugin` pool: backs the raw `sql` capability. Full read + write
    // DML on data, but no schema DDL on core and no app-role powers. (`config:read`
    // runs on the PRIVILEGED pool - see its registration below.)
    let db = deps.system.db.clone();
    // PRIVILEGED pool (runs as the app role): backs the server-owned structured
    // host fns - `host.storage.*`, `host.submission.*`, and `config:write` -
    // which build plugin-scoped SQL the server controls and run outside the
    // plugin role's boundary (core DDL, `plugin_login_secret`).
    let privileged_db = deps.system.privileged_db.clone();

    hr.register("logger", |plugin_id| {
        Function::new(
            "log_info",
            [ValType::I64],
            [],
            UserData::new(plugin_id.to_string()),
            logger::log_info,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("storage", move |plugin_id| {
        Function::new(
            "store_set",
            [ValType::I64],
            [],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            storage::store_set,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("storage", move |plugin_id| {
        Function::new(
            "store_get",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            storage::store_get,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("storage", move |plugin_id| {
        Function::new(
            "store_compare_and_set",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            storage::store_compare_and_set,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("storage", move |plugin_id| {
        Function::new(
            "store_delete",
            [ValType::I64],
            [],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            storage::store_delete,
        )
    });

    // `timer` gates both functions behind a single manifest permission: a
    // plugin without `"timer"` never sees `host.timer.schedule`/`.cancel` at
    // all, matching the `storage` capability's shape above. Runs on the
    // PRIVILEGED pool since `plugin_timer` is a server-owned core table, not
    // plugin-scoped data reachable through the restricted `broccoli_plugin`
    // role.
    let db_clone = privileged_db.clone();
    hr.register("timer", move |plugin_id| {
        Function::new(
            "timer_schedule",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            timer::timer_schedule,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("timer", move |plugin_id| {
        Function::new(
            "timer_cancel",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            timer::timer_cancel,
        )
    });

    let db_clone = db.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_execute",
            [ValType::I64, ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            sql::db_execute,
        )
    });

    let db_clone = db.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_query",
            [ValType::I64, ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            sql::db_query,
        )
    });

    let txn_map: sql::TransactionMap = Arc::new(StdMutex::new(HashMap::new()));

    let db_clone = db.clone();
    let txn_map_clone = txn_map.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_begin",
            [ValType::I64],
            [ValType::I64],
            UserData::new((
                plugin_id.to_string(),
                db_clone.clone(),
                txn_map_clone.clone(),
            )),
            sql::db_begin,
        )
    });

    let db_clone = db.clone();
    let txn_map_clone = txn_map.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_query_in",
            [ValType::I64, ValType::I64, ValType::I64],
            [ValType::I64],
            UserData::new((
                plugin_id.to_string(),
                db_clone.clone(),
                txn_map_clone.clone(),
            )),
            sql::db_query_in,
        )
    });

    let db_clone = db.clone();
    let txn_map_clone = txn_map.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_execute_in",
            [ValType::I64, ValType::I64, ValType::I64],
            [ValType::I64],
            UserData::new((
                plugin_id.to_string(),
                db_clone.clone(),
                txn_map_clone.clone(),
            )),
            sql::db_execute_in,
        )
    });

    let db_clone = db.clone();
    let txn_map_clone = txn_map.clone();
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_commit",
            [ValType::I64],
            [ValType::I64],
            UserData::new((
                plugin_id.to_string(),
                db_clone.clone(),
                txn_map_clone.clone(),
            )),
            sql::db_commit,
        )
    });

    let db_clone = db.clone();
    let txn_map_clone = txn_map;
    hr.register("sql", move |plugin_id| {
        Function::new(
            "db_rollback",
            [ValType::I64],
            [ValType::I64],
            UserData::new((
                plugin_id.to_string(),
                db_clone.clone(),
                txn_map_clone.clone(),
            )),
            sql::db_rollback,
        )
    });

    // Structured, capability-gated core-WRITE path (`host.submission.*`). These
    // build their SQL server-side from structured input; behind a distinct
    // `submission` capability, they run on the PRIVILEGED pool (the raw `sql`
    // channel above is now restricted to the read-only `broccoli_plugin` role,
    // phase 2), so their legitimate core writes keep working.
    let db_clone = privileged_db.clone();
    hr.register("submission", move |plugin_id| {
        Function::new(
            "submission_update",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            submissions::submission_update,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("submission", move |plugin_id| {
        Function::new(
            "submission_insert_results",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            submissions::submission_insert_results,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("submission", move |plugin_id| {
        Function::new(
            "submission_delete_results",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            submissions::submission_delete_results,
        )
    });

    let db_clone = privileged_db.clone();
    hr.register("submission", move |plugin_id| {
        Function::new(
            "submission_query_test_cases",
            [ValType::I64],
            [ValType::I64],
            UserData::new((plugin_id.to_string(), db_clone.clone())),
            submissions::submission_query_test_cases,
        )
    });

    let contest_reg = deps.system.contest_type_registry.clone();
    let eval_reg = deps.system.evaluator_registry.clone();
    let checker_stage_reg = deps.system.checker_stage_registry.clone();
    let lang_reg = deps.system.language_resolver_registry.clone();
    hr.register_many("plugin:register", move |plugin_id| {
        registry::create_registry_functions(
            plugin_id.to_string(),
            contest_reg.clone(),
            eval_reg.clone(),
            checker_stage_reg.clone(),
            lang_reg.clone(),
        )
    });

    // Cap concurrent evaluator plugin calls per server. Default = number of cores,
    // but on hosts with spare RAM and a high-fan-out workload (Signpost: 20 testcases
    // per submission, ICPC fans them out as parallel tokio tasks), this should be
    // bumped well above core count via `BROCCOLI__PLUGIN__EVALUATOR_PARALLELISM`.
    let evaluator_parallelism = deps.system.config.plugin.resolve_evaluator_parallelism();
    let fanout_concurrency = deps.system.config.server.batch_evaluator_fanout_concurrency as usize;
    tracing::info!(
        evaluator_parallelism,
        fanout_concurrency,
        "evaluator and fan-out semaphores configured"
    );
    let evaluator_slots = Arc::new(Semaphore::new(evaluator_parallelism));
    let fanout_slots = crate::dispatcher::fanout::FanoutSemaphore::new(
        fanout_concurrency,
        deps.system.metrics.clone(),
    );
    let eval_deps = deps.evaluate_deps(evaluator_slots, fanout_slots);
    hr.register_many("evaluator:evaluate", move |plugin_id| {
        evaluate::create_evaluate_functions(plugin_id.to_string(), eval_deps.clone())
    });

    let dispatch_deps = deps.operation_deps();
    hr.register_many("operations:dispatch", move |plugin_id| {
        dispatch::create_dispatch_functions(plugin_id.to_string(), dispatch_deps.clone())
    });

    let lang_reg = deps.system.language_resolver_registry;
    let pm_for_resolve = deps.plugin_manager.clone();
    hr.register("language:resolve", move |plugin_id| {
        language::create_resolve_language_function(
            plugin_id.to_string(),
            lang_reg.clone(),
            pm_for_resolve.clone(),
        )
    });

    let checker_stage_reg_for_resolve = deps.system.checker_stage_registry.clone();
    let pm_for_resolve_checker = deps.plugin_manager.clone();
    hr.register("checker:resolve", move |plugin_id| {
        checker::create_resolve_checker_function(
            plugin_id.to_string(),
            checker_stage_reg_for_resolve.clone(),
            pm_for_resolve_checker.clone(),
        )
    });

    let checker_stage_reg_for_interpret = deps.system.checker_stage_registry.clone();
    let pm_for_interpret_checker = deps.plugin_manager.clone();
    hr.register("checker:interpret", move |plugin_id| {
        checker::create_interpret_checker_function(
            plugin_id.to_string(),
            checker_stage_reg_for_interpret.clone(),
            pm_for_interpret_checker.clone(),
        )
    });

    // `config:read` reads `plugin_config`, which raw plugin SQL can no longer
    // SELECT (SELECT is revoked from `broccoli_plugin` so one plugin cannot read
    // another's config via raw SQL). It runs on the PRIVILEGED pool like
    // `config:write`: the SQL is server-owned and plugin-scoped (the row key's
    // namespace is always prefixed with the caller's `plugin_id`, and
    // `validate_raw_namespace` forbids `:` so the prefix cannot be forged), so a
    // plugin can still only read its OWN config.
    let db_clone = privileged_db.clone();
    let registry = deps.plugin_manager.get_registry().clone();
    hr.register("config:read", move |plugin_id| {
        config::create_config_get_function(
            plugin_id.to_string(),
            db_clone.clone(),
            registry.clone(),
        )
    });

    // `config:write` mutates the core `plugin_config` table, so - like
    // `host.submission.*` and `host.storage.*` - it runs on the PRIVILEGED pool.
    let priv_clone = privileged_db;
    hr.register("config:write", move |plugin_id| {
        config::create_config_set_function(plugin_id.to_string(), priv_clone.clone())
    });

    hr
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `record_block_in_place_regression` is `#[allow(dead_code)]` and
    /// has no production callers; without this test it would never be
    /// exercised. The `None` branch must be a no-op (calls into a
    /// missing `Metrics` would panic and bring down a host function).
    #[test]
    fn record_block_in_place_regression_no_metrics_is_noop() {
        record_block_in_place_regression(&None, "test_host_fn");
    }

    /// With a real `Metrics` instance, the recorder must run cleanly
    /// (we can't easily assert the counter incremented without an
    /// OTLP harness - non-panic is the contract under test here).
    #[test]
    fn record_block_in_place_regression_with_metrics_runs() {
        let _guard = crate::metrics_test_lock();
        let (metrics, _registry) = common::observability::init_metrics("broccoli-test");
        record_block_in_place_regression(&Some(metrics), "test_host_fn");
    }

    #[test]
    fn host_fn_span_uses_stable_name() {
        let span = host_fn_span("db_query", "test-plugin");
        assert_eq!(span.metadata().map(|m| m.name()), Some("host_fn"));
    }
}
