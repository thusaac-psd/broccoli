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

    fn read<R>(&self, f: impl FnOnce(&Counts) -> R) -> R {
        f(&self.counts.lock())
    }
}

struct TestManager {
    state: PluginManagerState,
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
    fixture.update(|c| c.running -= 1);
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
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let config = PluginConfig {
            plugins_dir: dir.path().to_path_buf(),
            ..PluginConfig::default()
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
                ]
            });
            TestManager {
                state: PluginManagerState::with_executor(config, executor),
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
    let h = Harness::new(4);
    let calls: Vec<_> = (0..500)
        .map(|_| h.spawn("hold", CallOptions::interactive(later())))
        .collect();
    until(|| {
        h.fixture.read(|c| c.running) == 4 && h.admission().queued(CallClass::Interactive) == 496
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
async fn a_queued_call_leaves_the_queue_with_its_request() {
    let h = Harness::new(2);
    let holders: Vec<_> = (0..2)
        .map(|_| h.spawn("hold", CallOptions::background(later())))
        .collect();
    until(|| h.fixture.read(|c| c.running) == 2).await;
    let request = h.spawn("mark", CallOptions::interactive(later()));
    until(|| h.admission().queued(CallClass::Interactive) == 1).await;
    request.abort();
    until(|| h.admission().queued(CallClass::Interactive) == 0).await;
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
    let deadline = Instant::now() + Duration::from_millis(50);
    let call = h.spawn("park_spin", CallOptions::background(deadline));
    until(|| h.fixture.read(|c| c.parked) == 1).await;
    until(|| Instant::now() >= deadline).await;
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
    assert_eq!(depth, 2, "the top-level call and one nested call ran");
    assert!(
        first_error.is_some_and(|e| e.contains("nests deeper than 2")),
        "the third level was refused"
    );
    assert_eq!(h.pool_count(), 2, "the nested call leased its own instance");
    assert_eq!(h.admission().running(CallClass::Background), 0);
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
    h.manager.reload_plugin(PLUGIN).unwrap();
    h.fixture.release();
    queued.await.unwrap().unwrap();
    for call in holders {
        call.await.unwrap().unwrap();
    }
    assert_eq!(h.fixture.read(|c| c.marks.clone()), vec![2]);
}
