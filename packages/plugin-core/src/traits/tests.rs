//! The execution model end to end, on real guests whose host functions the
//! test controls: `hold` stands for guest code running (it holds its token
//! until released), `park` for a host function that blocks (parked, holding
//! no token), `nest` calls back into the plugin, `mark` records the guest's
//! version.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use extism::{CurrentPlugin, Function, UserData, Val, ValType};
use parking_lot::{Condvar, Mutex};

use super::*;
use crate::admission::Capacity;
use crate::manager::PluginManagerState;
use crate::memory::MemoryBudget;

const PLUGIN: &str = "p";

fn guest(version: i64) -> String {
    format!(
        r#"(module
        (import "extism:host/user" "hold" (func $hold (param i64) (result i64)))
        (import "extism:host/user" "park" (func $park (param i64) (result i64)))
        (import "extism:host/user" "nest" (func $nest (param i64) (result i64)))
        (import "extism:host/user" "mark" (func $mark (param i64) (result i64)))
        (import "extism:host/user" "await_child" (func $await_child (param i64) (result i64)))
        (import "extism:host/user" "busy" (func $busy (param i64) (result i64)))
        (import "extism:host/user" "db_down" (func $db_down (param i64) (result i64)))
        (import "extism:host/user" "nest_hold" (func $nest_hold (param i64) (result i64)))
        (import "extism:host/user" "nest_spin" (func $nest_spin (param i64) (result i64)))
        (func (export "nest_spin") (result i32) (drop (call $nest_spin (i64.const 0))) (i32.const 0))
        (func (export "busy") (result i32) (drop (call $busy (i64.const 0))) (i32.const 0))
        (func (export "db_down") (result i32) (drop (call $db_down (i64.const 0))) (i32.const 0))
        (func (export "nest_hold") (result i32) (drop (call $nest_hold (i64.const 0))) (i32.const 0))
        (func (export "parent") (result i32) (drop (call $await_child (i64.const 0))) (i32.const 0))
        (func (export "hold") (result i32) (drop (call $hold (i64.const 0))) (i32.const 0))
        (func (export "park") (result i32) (drop (call $park (i64.const 0))) (i32.const 0))
        (func (export "nest") (result i32) (drop (call $nest (i64.const 0))) (i32.const 0))
        (func (export "mark") (result i32) (drop (call $mark (i64.const {version}))) (i32.const 0))
        (func (export "spin") (result i32) (loop (br 0)) (i32.const 0))
        (func (export "park_spin") (result i32)
            (drop (call $park (i64.const 0))) (loop (br 0)) (i32.const 0)))"#
    )
}

#[derive(Default)]
struct Counts {
    running: usize,
    max_running: usize,
    parked: usize,
    released: bool,
    nest_depth: usize,
    max_nest_depth: usize,
    nest_errors: Vec<String>,
    marks: Vec<i64>,
    /// Whether each `hold` found its instance retired once released.
    retired: Vec<bool>,
    /// Opened by the test to let `nest_hold` make its nested call.
    nest_open: bool,
}

/// What the host functions report to the test, and the gate they wait on.
#[derive(Default)]
struct Fixture {
    counts: Mutex<Counts>,
    changed: Condvar,
}

impl Fixture {
    fn update(&self, f: impl FnOnce(&mut Counts)) {
        f(&mut self.counts.lock());
        self.changed.notify_all();
    }

    fn wait_released(&self) {
        let mut counts = self.counts.lock();
        while !counts.released {
            self.changed.wait(&mut counts);
        }
    }

    fn release(&self) {
        self.update(|c| c.released = true);
    }

    fn wait_nest_open(&self) {
        let mut counts = self.counts.lock();
        while !counts.nest_open {
            self.changed.wait(&mut counts);
        }
    }

    fn read<R>(&self, f: impl FnOnce(&Counts) -> R) -> R {
        f(&self.counts.lock())
    }
}

struct TestManager {
    state: PluginManagerState,
    metrics: Option<common::metrics::Metrics>,
    host_functions: HostFunctionRegistry,
    i18n: I18nRegistry,
}

impl PluginInvoker for TestManager {
    fn get_registry(&self) -> &PluginRegistry {
        &self.state.registry
    }
    fn get_config(&self) -> &PluginConfig {
        &self.state.config
    }
    fn executor(&self) -> &Executor {
        &self.state.executor
    }
    fn get_metrics(&self) -> Option<&common::metrics::Metrics> {
        self.metrics.as_ref()
    }
}

impl PluginManager for TestManager {
    fn get_host_functions(&self) -> &HostFunctionRegistry {
        &self.host_functions
    }
    fn get_i18n_registry(&self) -> &I18nRegistry {
        &self.i18n
    }
    fn resolve(&self, manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
        manifest
            .server
            .as_ref()
            .map(|s| (s.entry.clone(), s.permissions.clone()))
    }
}

type Ctx = (Arc<Fixture>, Weak<TestManager>);

fn host_fn(
    name: &'static str,
    ctx: Ctx,
    f: fn(&Ctx, i64) -> Result<(), extism::Error>,
) -> Function {
    Function::new(
        name,
        [ValType::I64],
        [ValType::I64],
        UserData::new(ctx),
        move |_: &mut CurrentPlugin, inputs: &[Val], outputs: &mut [Val], data: UserData<Ctx>| {
            let ctx = data.get()?.lock().unwrap().clone();
            f(&ctx, inputs[0].unwrap_i64())?;
            outputs[0] = Val::I64(0);
            Ok(())
        },
    )
}

fn hold(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let fixture = &ctx.0;
    fixture.update(|c| {
        c.running += 1;
        c.max_running = c.max_running.max(c.running);
    });
    fixture.wait_released();
    let retired = crate::executor::instance_retired();
    fixture.update(|c| {
        c.running -= 1;
        c.retired.push(retired);
    });
    Ok(())
}

fn park(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let fixture = &ctx.0;
    crate::executor::parked(|| {
        fixture.update(|c| c.parked += 1);
        fixture.wait_released();
        fixture.update(|c| c.parked -= 1);
    });
    Ok(())
}

fn nest(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let (fixture, manager) = ctx;
    let manager = manager.upgrade().expect("manager alive");
    fixture.update(|c| {
        c.nest_depth += 1;
        c.max_nest_depth = c.max_nest_depth.max(c.nest_depth);
    });
    let result = crate::executor::parked(|| {
        tokio::runtime::Handle::current().block_on(manager.call_raw(
            PLUGIN,
            "nest",
            vec![],
            CallOptions::nested(),
        ))
    });
    fixture.update(|c| c.nest_depth -= 1);
    result.map(|_| ()).map_err(|e| {
        fixture.update(|c| c.nest_errors.push(e.to_string()));
        extism::Error::msg(e.to_string())
    })
}

/// Parks, waits until the test opens it, then calls `hold` as a nested
/// call.
fn nest_hold(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let (fixture, manager) = ctx;
    let manager = manager.upgrade().expect("manager alive");
    crate::executor::parked(|| {
        fixture.update(|c| c.parked += 1);
        fixture.wait_nest_open();
        fixture.update(|c| c.parked -= 1);
        tokio::runtime::Handle::current().block_on(manager.call_raw(
            PLUGIN,
            "hold",
            vec![],
            CallOptions::nested(),
        ))
    })
    .map(|_| ())
    .map_err(|e| extism::Error::msg(e.to_string()))
}

/// A contest call waiting for its verdicts: parks and waits for a separate
/// top-level background call, as `on_submission` waits for the evaluator
/// calls it started.
fn await_child(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let manager = ctx.1.upgrade().expect("manager alive");
    crate::executor::parked(|| {
        tokio::runtime::Handle::current().block_on(manager.call_raw(
            PLUGIN,
            "mark",
            vec![],
            CallOptions::background(later()),
        ))
    })
    .map(|_| ())
    .map_err(|e| extism::Error::msg(e.to_string()))
}

/// A host function that calls back into a guest that never returns, so the
/// nested call ends at its own deadline, and reports its failure as any host
/// function does, without marking the call.
fn nest_spin(ctx: &Ctx, _: i64) -> Result<(), extism::Error> {
    let manager = ctx.1.upgrade().expect("manager alive");
    crate::executor::parked(|| {
        tokio::runtime::Handle::current().block_on(manager.call_raw(
            PLUGIN,
            "spin",
            vec![],
            CallOptions::nested(),
        ))
    })
    .map(|_| ())
    .map_err(|e| extism::Error::msg(e.to_string()))
}

/// A host function that cannot serve the call right now.
fn busy(_: &Ctx, _: i64) -> Result<(), extism::Error> {
    crate::executor::fail_unavailable();
    Err(extism::Error::msg(
        "the host cannot serve this call right now",
    ))
}

/// A host function whose database is unreachable. It marks the call busy
/// first, as a host function deeper in the same call might: the database
/// outage is the cause the call reports.
fn db_down(_: &Ctx, _: i64) -> Result<(), extism::Error> {
    crate::executor::fail_database_unavailable();
    crate::executor::fail_unavailable();
    Err(extism::Error::msg("the database is unreachable"))
}

fn mark(ctx: &Ctx, version: i64) -> Result<(), extism::Error> {
    ctx.0.update(|c| c.marks.push(version));
    Ok(())
}

struct Harness {
    manager: Arc<TestManager>,
    fixture: Arc<Fixture>,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new(tokens: usize) -> Self {
        Self::with_capacity(Capacity::new(tokens))
    }

    fn with_capacity(capacity: Capacity) -> Self {
        Self::build(capacity, None)
    }

    fn build(capacity: Capacity, metrics: Option<common::metrics::Metrics>) -> Self {
        Self::build_with(capacity, metrics, PluginConfig::default())
    }

    fn build_with(
        capacity: Capacity,
        metrics: Option<common::metrics::Metrics>,
        config: PluginConfig,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let config = PluginConfig {
            plugins_dir: dir.path().to_path_buf(),
            cache_dir: dir.path().join(".cache"),
            ..config
        };
        let executor = Executor::new(capacity, MemoryBudget::new(u64::MAX, Box::new(|| 0)));
        let manager = Arc::new_cyclic(|weak: &Weak<TestManager>| {
            let mut host_functions = HostFunctionRegistry::new();
            let ctx: Ctx = (fixture.clone(), weak.clone());
            host_functions.register_many("test", move |_| {
                vec![
                    host_fn("hold", ctx.clone(), hold),
                    host_fn("park", ctx.clone(), park),
                    host_fn("nest", ctx.clone(), nest),
                    host_fn("mark", ctx.clone(), mark),
                    host_fn("await_child", ctx.clone(), await_child),
                    host_fn("busy", ctx.clone(), busy),
                    host_fn("db_down", ctx.clone(), db_down),
                    host_fn("nest_hold", ctx.clone(), nest_hold),
                    host_fn("nest_spin", ctx.clone(), nest_spin),
                ]
            });
            TestManager {
                state: PluginManagerState::with_executor(config, executor),
                metrics: metrics.clone(),
                host_functions,
                i18n: I18nRegistry::new(),
            }
        });
        let harness = Self {
            manager,
            fixture,
            dir,
        };
        harness.write_guest(1);
        harness.manager.discover_plugins().unwrap();
        harness.manager.load_plugin(PLUGIN).unwrap();
        harness
    }

    fn write_guest(&self, version: i64) {
        let root = self.dir.path().join(PLUGIN);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("plugin.toml"),
            "name = \"p\"\nversion = \"0.1.0\"\n\n[server]\nentry = \"guest.wat\"\npermissions = [\"test\"]\n",
        )
        .unwrap();
        std::fs::write(root.join("guest.wat"), guest(version)).unwrap();
    }

    fn admission(&self) -> &crate::admission::Admission {
        self.manager.executor().admission()
    }

    fn spawn(
        &self,
        func: &'static str,
        options: CallOptions,
    ) -> tokio::task::JoinHandle<Result<Vec<u8>, PluginError>> {
        let manager = self.manager.clone();
        tokio::spawn(async move { manager.call_raw(PLUGIN, func, vec![], options).await })
    }

    fn pool_count(&self) -> usize {
        let registry = self.manager.get_registry().read().unwrap();
        registry[PLUGIN].runtime.as_ref().unwrap().count()
    }

    /// A reload as the host's lifecycle does it: a fresh entry, a load, and
    /// the fresh version taking over from the one it replaced.
    fn reload(&self) {
        let fresh = crate::registry::PluginEntry::from_dir(&self.dir.path().join(PLUGIN)).unwrap();
        let previous = self
            .manager
            .get_registry()
            .write()
            .unwrap()
            .insert(PLUGIN.to_string(), fresh)
            .unwrap();
        self.manager.load_plugin(PLUGIN).unwrap();
        self.manager
            .get_registry()
            .write()
            .unwrap()
            .get_mut(PLUGIN)
            .unwrap()
            .succeed(&previous);
    }

    /// How many replaced pools the plugin's entry keeps.
    fn superseded(&self) -> usize {
        self.manager.get_registry().read().unwrap()[PLUGIN].superseded_count()
    }
}

impl Drop for Harness {
    /// Lets every held call finish, so a failed assertion cannot leave the
    /// runtime waiting on blocked threads.
    fn drop(&mut self) {
        self.fixture.release();
    }
}

fn later() -> Instant {
    Instant::now() + Duration::from_secs(600)
}

/// Yield until `ready` holds. The state it reads is driven by the calls
/// under test, never by a clock.
async fn until(ready: impl Fn() -> bool) {
    while !ready() {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_never_runs_more_guests_than_tokens() {
    let h = Harness::new(4);
    let calls: Vec<_> = (0..12)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| {
        h.fixture.read(|c| c.running) == 4 && h.admission().queued(CallClass::Background) == 8
    })
    .await;
    h.fixture.release();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert_eq!(h.fixture.read(|c| c.max_running), 4);
    assert_eq!(h.admission().running(CallClass::Background), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_parked_in_a_host_wait_holds_no_token() {
    // Like a burst of contest submissions, each parked for its whole
    // judgement: far more than the tokens, all parked at once.
    let h = Harness::new(4);
    let calls: Vec<_> = (0..16)
        .map(|_| h.spawn("park", CallOptions::judging(later())))
        .collect();
    until(|| h.fixture.read(|c| c.parked) == 16).await;
    assert_eq!(h.admission().running(CallClass::Background), 0);
    h.fixture.release();
    for call in calls {
        call.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threads_stay_bounded_under_a_slow_plugin_flood() {
    let h =
        Harness::with_capacity(Capacity::new(4).with_cap(crate::admission::CallKind::Work, 1000));
    let calls: Vec<_> = (0..500)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| {
        h.fixture.read(|c| c.running) == 4 && h.admission().queued(CallClass::Background) == 496
    })
    .await;
    // One guest thread per running guest; the 496 queued calls hold none.
    assert_eq!(h.manager.executor().threads().live(), 4);
    h.fixture.release();
    for call in calls {
        call.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_function_fills_only_its_own_lane() {
    let h = Harness::new(4);
    let lane_cap = h.admission().capacity().lane_cap();
    // Request calls of a function that hold their guest: past its lane's cap
    // they queue (or are refused), however many tokens are free.
    let slow: Vec<_> = (0..6)
        .map(|_| h.spawn("hold", CallOptions::interactive(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == lane_cap).await;
    // A call to another function of the same plugin runs at once.
    h.manager
        .call_raw(PLUGIN, "mark", vec![], CallOptions::interactive(later()))
        .await
        .expect("a call to another function runs beside the full lane");
    h.fixture.release();
    for call in slow {
        match call.await.unwrap() {
            Ok(_) | Err(PluginError::Unavailable { .. }) => {}
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(h.fixture.read(|c| c.max_running), lane_cap);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contest_calls_at_their_cap_waiting_on_child_calls_all_complete() {
    // Two tokens, three contest calls admitted at once, a small work cap:
    // the shape of a server whose in-flight cap is full of contest calls
    // parked for their verdicts.
    let capacity = Capacity::new(2)
        .with_cap(crate::admission::CallKind::Judging, 3)
        .with_cap(crate::admission::CallKind::Work, 2);
    let threads = capacity.caps.threads();
    let h = Harness::with_capacity(capacity);
    let parents: Vec<_> = (0..9)
        .map(|_| h.spawn("parent", CallOptions::judging(later())))
        .collect();
    for parent in parents {
        parent.await.unwrap().unwrap();
    }
    assert_eq!(h.fixture.read(|c| c.marks.len()), 9, "every child ran");
    assert!(h.manager.executor().threads().live() <= threads);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_call_leaves_the_queue_when_its_caller_goes_away() {
    let h = Harness::new(2);
    let holders: Vec<_> = (0..2)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == 2).await;
    // A background call: it queues until a token frees, however long.
    let caller = h.spawn("mark", CallOptions::background(later()));
    until(|| h.admission().queued(CallClass::Background) == 1).await;
    caller.abort();
    until(|| h.admission().queued(CallClass::Background) == 0).await;
    h.fixture.release();
    for call in holders {
        call.await.unwrap().unwrap();
    }
    assert!(
        h.fixture.read(|c| c.marks.is_empty()),
        "the dropped call never ran"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_still_queued_at_its_deadline_is_unavailable() {
    let h = Harness::new(2);
    let holders: Vec<_> = (0..2)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == 2).await;
    let err = h
        .manager
        .call_raw(
            PLUGIN,
            "mark",
            vec![],
            CallOptions::interactive(Instant::now()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Unavailable { .. }), "{err:?}");
    h.fixture.release();
    for call in holders {
        call.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deadline_cancels_a_spinning_guest_and_frees_its_token() {
    let h = Harness::new(2);
    let err = h
        .manager
        .call_raw(
            PLUGIN,
            "spin",
            vec![],
            CallOptions::interactive(Instant::now() + Duration::from_millis(50)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Unavailable { .. }), "{err:?}");
    assert_eq!(h.admission().running(CallClass::Interactive), 0);
    h.manager
        .call_raw(PLUGIN, "mark", vec![], CallOptions::interactive(later()))
        .await
        .expect("the instance runs its next call");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_parked_past_its_deadline_is_cancelled_when_it_resumes() {
    let h = Harness::new(2);
    // The deadline passes once the guest is parked, at a point the test
    // chooses: a clock deadline could pass before the guest reaches the park.
    let deadline = later();
    let call = h.spawn("park_spin", CallOptions::background(deadline));
    until(|| h.fixture.read(|c| c.parked) == 1).await;
    crate::deadline::expire(deadline);
    h.fixture.release();
    let err = call.await.unwrap().unwrap_err();
    assert!(matches!(err, PluginError::Unavailable { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nesting_stops_at_the_depth_cap_on_a_separate_instance() {
    let h = Harness::new(2);
    let err = h
        .manager
        .call_raw(PLUGIN, "nest", vec![], CallOptions::background(later()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PluginError::ExecutionFailed { .. }),
        "{err:?}"
    );
    let (depth, first_error) = h
        .fixture
        .read(|c| (c.max_nest_depth, c.nest_errors.first().cloned()));
    assert_eq!(depth, 3, "the top-level call and two nested calls ran");
    assert!(
        first_error.is_some_and(|e| e.contains("nests deeper than 3")),
        "the fourth level was refused"
    );
    assert_eq!(
        h.pool_count(),
        3,
        "each nested call leased its own instance"
    );
    assert_eq!(h.admission().running(CallClass::Background), 0);
}

/// A failure deep in a chain of nested calls reaches the top-level caller
/// with the plugin, function and depth of every level and the innermost
/// reason, not the wasm backtrace the host function's error was wrapped in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_nested_call_reports_every_level_and_the_reason() {
    let h = Harness::new(2);
    let err = h
        .manager
        .call_raw(PLUGIN, "nest", vec![], CallOptions::background(later()))
        .await
        .unwrap_err();
    let PluginError::ExecutionFailed { depth, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*depth, 1);
    let message = err.to_string();
    for level in 1..=3 {
        assert!(
            message.contains(&format!("on plugin '{PLUGIN}' at call depth {level}")),
            "level {level} is named: {message}"
        );
    }
    assert!(
        message.ends_with("nests deeper than 3 plugin calls"),
        "the innermost reason comes last: {message}"
    );
    assert!(!message.contains("wasm backtrace"), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_call_outside_a_plugin_call_is_refused() {
    let h = Harness::new(2);
    let err = h
        .manager
        .call_raw(PLUGIN, "mark", vec![], CallOptions::nested())
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Internal(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lifecycle_call_runs_while_every_token_is_taken() {
    let h = Harness::new(2);
    let holders: Vec<_> = (0..2)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == 2).await;
    h.manager
        .call_raw(PLUGIN, "mark", vec![], CallOptions::lifecycle(later()))
        .await
        .unwrap();
    h.fixture.release();
    for call in holders {
        call.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_queued_across_a_reload_runs_on_the_new_version() {
    let h = Harness::new(2);
    let holders: Vec<_> = (0..2)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == 2).await;
    let queued = h.spawn("mark", CallOptions::background(later()));
    until(|| h.admission().queued(CallClass::Background) == 1).await;
    h.write_guest(2);
    // A reload as the host's lifecycle does it: a fresh entry, then a load.
    let fresh = crate::registry::PluginEntry::from_dir(&h.dir.path().join(PLUGIN)).unwrap();
    h.manager
        .get_registry()
        .write()
        .unwrap()
        .insert(PLUGIN.to_string(), fresh);
    h.manager.load_plugin(PLUGIN).unwrap();
    h.fixture.release();
    queued.await.unwrap().unwrap();
    for call in holders {
        call.await.unwrap().unwrap();
    }
    assert_eq!(h.fixture.read(|c| c.marks.clone()), vec![2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_retires_the_instance_a_running_call_holds() {
    let h = Harness::new(2);
    let running = h.spawn("hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    h.manager.unload_plugin(PLUGIN).unwrap();
    h.fixture.release();
    running.await.unwrap().unwrap();
    assert_eq!(h.fixture.read(|c| c.retired.clone()), vec![true]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_keeps_the_instance_a_running_call_holds_live() {
    let h = Harness::new(2);
    let running = h.spawn("hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    h.write_guest(2);
    // A reload as the host's lifecycle does it: a fresh entry, then a load.
    let fresh = crate::registry::PluginEntry::from_dir(&h.dir.path().join(PLUGIN)).unwrap();
    h.manager
        .get_registry()
        .write()
        .unwrap()
        .insert(PLUGIN.to_string(), fresh);
    h.manager.load_plugin(PLUGIN).unwrap();
    h.fixture.release();
    running.await.unwrap().unwrap();
    assert_eq!(h.fixture.read(|c| c.retired.clone()), vec![false]);
}

/// A call that started before a reload still runs on the version the
/// reload replaced; the plugin's unload retires it all the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_retires_a_call_still_running_on_a_version_a_reload_replaced() {
    let h = Harness::new(2);
    let running = h.spawn("hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    h.write_guest(2);
    h.reload();
    h.manager.unload_plugin(PLUGIN).unwrap();
    h.fixture.release();
    running.await.unwrap().unwrap();
    assert_eq!(h.fixture.read(|c| c.retired.clone()), vec![true]);
}

/// A reload keeps only the replaced pools a call still runs on: with no
/// call running nothing is kept, and a pool whose last call ended is
/// dropped at the next reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_keeps_only_the_replaced_pools_a_call_still_runs_on() {
    let h = Harness::new(2);
    h.reload();
    assert_eq!(h.superseded(), 0, "no call ran on the replaced pool");
    let running = h.spawn("hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    h.reload();
    assert_eq!(h.superseded(), 1, "a call still runs on the replaced pool");
    h.fixture.release();
    running.await.unwrap().unwrap();
    h.reload();
    assert_eq!(h.superseded(), 0, "its call ended");
}

/// A call holds its version from the moment it looks the pool up, before
/// its guest thread enters the call: a reload in between keeps that version
/// with the plugin, and the unload that follows retires it. The nested call
/// here has looked up v2 and waits for a token when v2 is replaced and the
/// plugin unloaded; it never runs on v2 un-retired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_between_its_lookup_and_its_guest_is_retired_by_a_later_unload() {
    let h = Harness::new(crate::admission::MIN_TOKENS);
    // v1: a parent call parks in a host function, giving its token up.
    let parent = h.spawn("nest_hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.parked) == 1).await;
    // Other calls take every token and hold them.
    let holding = [
        h.spawn("hold", CallOptions::background(later())),
        h.spawn("hold", CallOptions::interactive(later())),
    ];
    until(|| h.fixture.read(|c| c.running) == 2).await;
    // v2, then the parent's nested call looks v2 up and waits for the token.
    h.write_guest(2);
    h.reload();
    h.fixture.update(|c| c.nest_open = true);
    until(|| h.admission().resuming(CallClass::Background) == 1).await;
    // v3 replaces v2 while the nested call waits, then the plugin unloads.
    h.write_guest(3);
    h.reload();
    h.manager.unload_plugin(PLUGIN).unwrap();
    h.fixture.release();
    for call in holding {
        call.await.unwrap().unwrap();
    }
    let _ = parent.await.unwrap();
    assert!(
        !h.fixture.read(|c| c.retired.clone()).contains(&false),
        "a call ran on a replaced version the unload did not retire: {:?}",
        h.fixture.read(|c| c.retired.clone())
    );
}

/// A reload that does not take is dropped alone: a call running on it is
/// retired, and the version put back keeps serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_discarded_version_retires_its_calls_and_spares_the_one_put_back() {
    let h = Harness::new(2);
    h.write_guest(2);
    let fresh = crate::registry::PluginEntry::from_dir(&h.dir.path().join(PLUGIN)).unwrap();
    let previous = h
        .manager
        .get_registry()
        .write()
        .unwrap()
        .insert(PLUGIN.to_string(), fresh)
        .unwrap();
    h.manager.load_plugin(PLUGIN).unwrap();
    let running = h.spawn("hold", CallOptions::background(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    let refused = h
        .manager
        .get_registry()
        .write()
        .unwrap()
        .insert(PLUGIN.to_string(), previous)
        .unwrap();
    refused.discard();
    h.fixture.release();
    running.await.unwrap().unwrap();
    assert_eq!(h.fixture.read(|c| c.retired.clone()), vec![true]);
    h.spawn("mark", CallOptions::background(later()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.fixture.read(|c| c.marks.clone()), vec![1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_request_is_counted_in_flight_until_its_guest_returns() {
    let (metrics, registry) = common::observability::init_metrics("plugin-core-in-flight-test");
    let h = Harness::build(Capacity::new(2), Some(metrics));
    let in_flight = || -> f64 {
        registry
            .gather()
            .iter()
            .filter(|f| f.name() == "broccoli_plugin_calls_in_flight")
            .flat_map(|f| f.get_metric().iter().map(|m| m.get_gauge().value()))
            .sum()
    };
    let request = h.spawn("hold", CallOptions::interactive(later()));
    until(|| h.fixture.read(|c| c.running) == 1).await;
    request.abort();
    let _ = request.await;
    assert_eq!(in_flight(), 1.0, "the guest still runs");
    h.fixture.release();
    until(|| in_flight() == 0.0).await;
}

/// A nested call the host cannot serve (here: it reached its own deadline,
/// which the call timeout bounds below the caller's) fails the call that made
/// it as unavailable too, whatever the host function in between reports, so
/// callers answer the whole call as "try again".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_whose_nested_call_is_unavailable_is_unavailable() {
    let config = PluginConfig {
        call_timeout_secs: 1,
        ..PluginConfig::default()
    };
    let h = Harness::build_with(Capacity::new(2), None, config);
    let err = h
        .manager
        .call_raw(
            PLUGIN,
            "nest_spin",
            vec![],
            CallOptions::background(later()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Unavailable { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_function_can_fail_its_call_as_unavailable() {
    let h = Harness::new(2);
    let err = h
        .manager
        .call_raw(PLUGIN, "busy", vec![], CallOptions::interactive(later()))
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Unavailable { .. }), "{err:?}");
    // A call that could not run for its database's outage says so.
    let err = h
        .manager
        .call_raw(PLUGIN, "db_down", vec![], CallOptions::interactive(later()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PluginError::DatabaseUnavailable { .. }),
        "{err:?}"
    );
    assert!(err.is_unavailable());
    // The mark belongs to that call alone.
    h.manager
        .call_raw(PLUGIN, "mark", vec![], CallOptions::interactive(later()))
        .await
        .expect("the next call runs");
}
