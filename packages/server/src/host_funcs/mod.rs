pub mod checker;
pub mod config;
pub mod context;
pub mod dispatch;
pub mod evaluate;
pub mod feedback;
pub mod language;
pub mod logger;
pub mod registry;
pub mod sql;
pub mod storage;
pub mod submissions;
pub mod timer;

use crate::host_funcs::context::HostFunctionDeps;
use extism::ValType::I64;
use extism::{CurrentPlugin, Function, UserData, Val, ValType};
use plugin_core::host::HostFunctionRegistry;
use sea_orm::DatabaseConnection;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tracing::Span;
use tracing::span::EnteredSpan;

/// Per-plugin state carried in a host fn's `UserData`.
///
/// Every host fn's context is fixed at registration time and names the plugin
/// it was resolved for, which is all the shared prologue needs to open the
/// `host_fn` span.
pub(crate) trait HostFnContext {
    fn plugin_id(&self) -> &str;
}

impl HostFnContext for String {
    fn plugin_id(&self) -> &str {
        self
    }
}

impl<A> HostFnContext for (String, A) {
    fn plugin_id(&self) -> &str {
        &self.0
    }
}

impl<A, B> HostFnContext for (String, A, B) {
    fn plugin_id(&self) -> &str {
        &self.0
    }
}

/// Wrap a host fn context for `Function::new`.
///
/// extism's `UserData` is an `Arc<Mutex<T>>` shared by every pooled instance of
/// the plugin. The context is immutable, so it is stored behind an `Arc`: the
/// prologue holds the mutex only long enough to bump the refcount, never across
/// the body's blocking DB or plugin call, and never clones the context itself.
pub(crate) fn host_data<C>(ctx: C) -> UserData<Arc<C>> {
    UserData::new(Arc::new(ctx))
}

/// Whether a host function waits on something outside the guest: the
/// database, the queue, a judging result or another plugin call. Every host
/// function declares it where it is built ([`host_function`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waits {
    /// The call is parked for the whole body: its CPU token goes to another
    /// call and comes back, ahead of its class's queued calls, when the
    /// body returns (see [`plugin_core::executor::parked`]).
    Blocks,
    /// The body returns at once (a log line, an in-memory registry update);
    /// the call keeps its token.
    Returns,
}

/// Build host function `name`, parking its call around the body when it
/// [`Waits::Blocks`].
pub(crate) fn host_function<T, F>(
    name: &'static str,
    params: impl IntoIterator<Item = ValType>,
    results: impl IntoIterator<Item = ValType>,
    user_data: UserData<T>,
    waits: Waits,
    f: F,
) -> Function
where
    T: 'static,
    F: Fn(&mut CurrentPlugin, &[Val], &mut [Val], UserData<T>) -> Result<(), extism::Error>
        + Send
        + Sync
        + 'static,
{
    match waits {
        Waits::Blocks => Function::new(
            name,
            params,
            results,
            user_data,
            move |plugin: &mut CurrentPlugin, inputs: &[Val], outputs: &mut [Val], data| {
                plugin_core::executor::parked(|| f(plugin, inputs, outputs, data))
            },
        ),
        Waits::Returns => Function::new(name, params, results, user_data, f),
    }
}

/// An in-flight host fn call: the snapshot of its context plus the entered
/// `host_fn` span, which closes when the guard is dropped at the end of the
/// body. Derefs to the context.
#[must_use = "dropping the guard closes the host_fn span"]
pub(crate) struct HostCall<C> {
    ctx: Arc<C>,
    _span: EnteredSpan,
}

impl<C: HostFnContext> HostCall<C> {
    /// The shared host fn prologue: snapshot the context out of `UserData`
    /// and enter the `host_fn` span named `host_fn` for its plugin.
    pub(crate) fn enter(
        user_data: &UserData<Arc<C>>,
        host_fn: &'static str,
    ) -> Result<Self, extism::Error> {
        let ctx = user_data
            .get()?
            .lock()
            .map_err(|_| extism::Error::msg("Lock poisoned"))?
            .clone();
        let span = host_fn_span(host_fn, ctx.plugin_id()).entered();
        Ok(Self { ctx, _span: span })
    }
}

impl<C> std::ops::Deref for HostCall<C> {
    type Target = C;

    fn deref(&self) -> &C {
        &self.ctx
    }
}

/// `wait`, cut short at the deadline of the plugin call this host function
/// serves: a guest asking for a long result wait cannot outwait its own
/// call.
pub(crate) fn within_call_deadline(wait: std::time::Duration) -> std::time::Duration {
    match plugin_core::executor::current_deadline() {
        Some(deadline) => wait.min(deadline.saturating_duration_since(std::time::Instant::now())),
        None => wait,
    }
}

/// Read a raw host fn's JSON input from guest memory at `inputs[0]`. `what`
/// names the input in the guest-visible error.
pub(crate) fn read_json<T: DeserializeOwned>(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    what: &str,
) -> Result<T, extism::Error> {
    let bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
    serde_json::from_slice(&bytes)
        .map_err(|e| extism::Error::msg(format!("Failed to deserialize {what}: {e}")))
}

/// Serialize `value` and hand it to the guest as a raw host fn's output.
pub(crate) fn write_json<T: Serialize + ?Sized>(
    plugin: &mut CurrentPlugin,
    outputs: &mut [Val],
    value: &T,
    what: &str,
) -> Result<(), extism::Error> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| extism::Error::msg(format!("Failed to serialize {what}: {e}")))?;
    write_bytes(plugin, outputs, &bytes)
}

/// Hand already-encoded bytes to the guest as a raw host fn's output.
pub(crate) fn write_bytes(
    plugin: &mut CurrentPlugin,
    outputs: &mut [Val],
    bytes: &[u8],
) -> Result<(), extism::Error> {
    let handle = plugin.memory_new(bytes)?;
    outputs[0] = Val::I64(handle.offset() as i64);
    Ok(())
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
    let db = &deps.system.db;
    // PRIVILEGED pool (runs as the app role): backs the server-owned structured
    // host fns - `host.storage.*`, `host.submission.*`, `host.timer.*` and
    // `config:*` - which build plugin-scoped SQL the server controls and run
    // outside the plugin role's boundary (core DDL, `plugin_login_secret`).
    let privileged_db = &deps.system.privileged_db;

    register_fn(
        &mut hr,
        "logger",
        "log_info",
        (&[I64], &[]),
        str::to_string,
        Waits::Returns,
        logger::log_info,
    );
    register_storage_functions(&mut hr, privileged_db);
    register_timer_functions(&mut hr, privileged_db);
    register_sql_functions(&mut hr, db);
    register_submission_functions(&mut hr, privileged_db);
    register_registry_functions(&mut hr, &deps);
    register_evaluation_functions(&mut hr, &deps);
    register_config_functions(&mut hr, &deps, privileged_db);
    hr
}

/// Register host fn `name` under `capability`. Each plugin's instance gets
/// `user_data(plugin_id)` as its context.
fn register_fn<T, F>(
    hr: &mut HostFunctionRegistry,
    capability: &str,
    name: &'static str,
    (params, results): (&[ValType], &[ValType]),
    user_data: impl Fn(&str) -> T + Send + Sync + 'static,
    waits: Waits,
    f: F,
) where
    T: Send + Sync + 'static,
    F: Fn(&mut CurrentPlugin, &[Val], &mut [Val], UserData<Arc<T>>) -> Result<(), extism::Error>
        + Clone
        + Send
        + Sync
        + 'static,
{
    let (params, results) = (params.to_vec(), results.to_vec());
    hr.register(capability, move |plugin_id| {
        host_function(
            name,
            params.clone(),
            results.clone(),
            host_data(user_data(plugin_id)),
            waits,
            f.clone(),
        )
    });
}

/// User data for a host fn bound to one plugin and a database pool.
fn with_db(
    db: &DatabaseConnection,
) -> impl Fn(&str) -> (String, DatabaseConnection) + Send + Sync + 'static {
    let db = db.clone();
    move |plugin_id| (plugin_id.to_string(), db.clone())
}

/// `host.storage.*`: plugin-scoped key-value storage, on the privileged pool.
fn register_storage_functions(hr: &mut HostFunctionRegistry, privileged_db: &DatabaseConnection) {
    let cap = "storage";
    register_fn(
        hr,
        cap,
        "store_set",
        (&[I64], &[]),
        with_db(privileged_db),
        Waits::Blocks,
        storage::store_set,
    );
    register_fn(
        hr,
        cap,
        "store_get",
        (&[I64], &[I64]),
        with_db(privileged_db),
        Waits::Blocks,
        storage::store_get,
    );
    register_fn(
        hr,
        cap,
        "store_compare_and_set",
        (&[I64], &[I64]),
        with_db(privileged_db),
        Waits::Blocks,
        storage::store_compare_and_set,
    );
    register_fn(
        hr,
        cap,
        "store_delete",
        (&[I64], &[]),
        with_db(privileged_db),
        Waits::Blocks,
        storage::store_delete,
    );
    register_fn(
        hr,
        cap,
        "store_scan",
        (&[I64], &[I64]),
        with_db(privileged_db),
        Waits::Blocks,
        storage::store_scan,
    );
}

/// `host.timer.*`. One manifest permission gates both functions, so a plugin
/// without `"timer"` never sees either. Runs on the PRIVILEGED pool since
/// `plugin_timer` is a server-owned core table, not plugin-scoped data
/// reachable through the restricted `broccoli_plugin` role.
fn register_timer_functions(hr: &mut HostFunctionRegistry, privileged_db: &DatabaseConnection) {
    let cap = "timer";
    register_fn(
        hr,
        cap,
        "timer_schedule",
        (&[I64], &[I64]),
        with_db(privileged_db),
        Waits::Blocks,
        timer::timer_schedule,
    );
    register_fn(
        hr,
        cap,
        "timer_cancel",
        (&[I64], &[I64]),
        with_db(privileged_db),
        Waits::Blocks,
        timer::timer_cancel,
    );
}

/// The raw `sql` capability on the restricted pool, including explicit
/// transactions shared across one plugin's calls through `txn_map`.
fn register_sql_functions(hr: &mut HostFunctionRegistry, db: &DatabaseConnection) {
    let cap = "sql";
    register_fn(
        hr,
        cap,
        "db_execute",
        (&[I64, I64], &[I64]),
        with_db(db),
        Waits::Blocks,
        sql::db_execute,
    );
    register_fn(
        hr,
        cap,
        "db_query",
        (&[I64, I64], &[I64]),
        with_db(db),
        Waits::Blocks,
        sql::db_query,
    );

    let txn_map: sql::TransactionMap = Arc::new(StdMutex::new(HashMap::new()));
    let with_txns = || {
        let (db, txn_map) = (db.clone(), txn_map.clone());
        move |plugin_id: &str| (plugin_id.to_string(), db.clone(), txn_map.clone())
    };
    register_fn(
        hr,
        cap,
        "db_begin",
        (&[I64], &[I64]),
        with_txns(),
        Waits::Blocks,
        sql::db_begin,
    );
    register_fn(
        hr,
        cap,
        "db_query_in",
        (&[I64, I64, I64], &[I64]),
        with_txns(),
        Waits::Blocks,
        sql::db_query_in,
    );
    register_fn(
        hr,
        cap,
        "db_execute_in",
        (&[I64, I64, I64], &[I64]),
        with_txns(),
        Waits::Blocks,
        sql::db_execute_in,
    );
    register_fn(
        hr,
        cap,
        "db_commit",
        (&[I64], &[I64]),
        with_txns(),
        Waits::Blocks,
        sql::db_commit,
    );
    register_fn(
        hr,
        cap,
        "db_rollback",
        (&[I64], &[I64]),
        with_txns(),
        Waits::Blocks,
        sql::db_rollback,
    );
}

/// `host.submission.*`: the structured, capability-gated core-WRITE path. It
/// builds its SQL server-side from structured input and runs on the
/// PRIVILEGED pool, since the raw `sql` channel is restricted to the
/// `broccoli_plugin` role.
fn register_submission_functions(
    hr: &mut HostFunctionRegistry,
    privileged_db: &DatabaseConnection,
) {
    let cap = "submission";
    let functions: [(&'static str, HostFnDb); 9] = [
        ("submission_update", submissions::submission_update),
        (
            "submission_insert_results",
            submissions::submission_insert_results,
        ),
        (
            "submission_delete_results",
            submissions::submission_delete_results,
        ),
        (
            "submission_query_test_cases",
            submissions::submission_query_test_cases,
        ),
        (
            "submission_feedback_query",
            feedback::submission_feedback_query,
        ),
        (
            "submission_feedback_update",
            feedback::submission_feedback_update,
        ),
        ("submission_slot_observe", feedback::submission_slot_observe),
        ("code_run_update", submissions::code_run_update),
        (
            "code_run_insert_results",
            submissions::code_run_insert_results,
        ),
    ];
    for (name, f) in functions {
        register_fn(
            hr,
            cap,
            name,
            (&[I64], &[I64]),
            with_db(privileged_db),
            Waits::Blocks,
            f,
        );
    }
}

/// A host fn whose context is the calling plugin and a database pool.
type HostFnDb = fn(
    &mut CurrentPlugin,
    &[Val],
    &mut [Val],
    UserData<Arc<(String, DatabaseConnection)>>,
) -> Result<(), extism::Error>;

/// `plugin:register` (contest types, evaluators, checker stages, languages)
/// and the `language:resolve` / `checker:*` lookups into those registries.
fn register_registry_functions(hr: &mut HostFunctionRegistry, deps: &HostFunctionDeps) {
    let system = &deps.system;
    let contest_reg = system.contest_type_registry.clone();
    let eval_reg = system.evaluator_registry.clone();
    let checker_stage_reg = system.checker_stage_registry.clone();
    let lang_reg = system.language_resolver_registry.clone();
    hr.register_many("plugin:register", move |plugin_id| {
        registry::create_registry_functions(
            plugin_id.to_string(),
            contest_reg.clone(),
            eval_reg.clone(),
            checker_stage_reg.clone(),
            lang_reg.clone(),
        )
    });

    let lang_reg = system.language_resolver_registry.clone();
    let pm = deps.plugin_manager.clone();
    hr.register("language:resolve", move |plugin_id| {
        language::create_resolve_language_function(
            plugin_id.to_string(),
            lang_reg.clone(),
            pm.clone(),
        )
    });

    let checker_stage_reg = system.checker_stage_registry.clone();
    let pm = deps.plugin_manager.clone();
    hr.register("checker:resolve", move |plugin_id| {
        checker::create_resolve_checker_function(
            plugin_id.to_string(),
            checker_stage_reg.clone(),
            pm.clone(),
        )
    });

    let checker_stage_reg = system.checker_stage_registry.clone();
    let pm = deps.plugin_manager.clone();
    hr.register("checker:interpret", move |plugin_id| {
        checker::create_interpret_checker_function(
            plugin_id.to_string(),
            checker_stage_reg.clone(),
            pm.clone(),
        )
    });
}

/// `evaluator:evaluate` and `operations:dispatch`. Test cases outstanding
/// at workers are bounded by the live worker slots ([`Fanout`]).
///
/// [`Fanout`]: crate::dispatcher::fanout::Fanout
fn register_evaluation_functions(hr: &mut HostFunctionRegistry, deps: &HostFunctionDeps) {
    let fanout = crate::dispatcher::fanout::Fanout::new(
        deps.system.worker_slots.clone(),
        deps.system.metrics.clone(),
    );
    let eval_deps = deps.evaluate_deps(fanout);
    hr.register_many("evaluator:evaluate", move |plugin_id| {
        evaluate::create_evaluate_functions(plugin_id.to_string(), eval_deps.clone())
    });

    let dispatch_deps = deps.operation_deps();
    hr.register_many("operations:dispatch", move |plugin_id| {
        dispatch::create_dispatch_functions(plugin_id.to_string(), dispatch_deps.clone())
    });
}

/// `config:read` and `config:write`, both on the PRIVILEGED pool.
///
/// `config:read` reads `plugin_config`, which raw plugin SQL cannot SELECT
/// (so one plugin cannot read another's config). The SQL is server-owned and
/// plugin-scoped: the row key's namespace is always prefixed with the
/// caller's `plugin_id`, and `validate_raw_namespace` forbids `:` so the
/// prefix cannot be forged, so a plugin can only read its OWN config.
/// `config:write` mutates the core `plugin_config` table.
fn register_config_functions(
    hr: &mut HostFunctionRegistry,
    deps: &HostFunctionDeps,
    privileged_db: &DatabaseConnection,
) {
    let db = privileged_db.clone();
    let registry = deps.plugin_manager.get_registry().clone();
    hr.register("config:read", move |plugin_id| {
        config::create_config_get_function(plugin_id.to_string(), db.clone(), registry.clone())
    });

    let db = privileged_db.clone();
    hr.register("config:write", move |plugin_id| {
        config::create_config_set_function(plugin_id.to_string(), db.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_call_snapshots_context_and_releases_the_lock() {
        let user_data = host_data(("test-plugin".to_string(), 7u32));
        let call = HostCall::enter(&user_data, "db_query").expect("prologue");
        assert_eq!(call.plugin_id(), "test-plugin");
        assert_eq!(call.1, 7);
        // The UserData mutex is not held for the body of the call.
        let shared = user_data.get().expect("rust user data");
        assert!(shared.try_lock().is_ok());
    }

    #[test]
    fn host_fn_span_uses_stable_name() {
        let span = host_fn_span("db_query", "test-plugin");
        assert_eq!(span.metadata().map(|m| m.name()), Some("host_fn"));
    }
}
