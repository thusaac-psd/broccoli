//! Admission to run guest code.
//!
//! Two limits govern every top-level plugin call:
//!
//! - **CPU tokens.** A server holds `C` tokens. A call holds one only while
//!   guest code runs: around every host function that blocks it is parked
//!   (gives its token back) and resumed (takes one back ahead of its class's
//!   queued calls). So at most `C` guests run at once, and a call parked for
//!   minutes in a host wait (a contest call waiting for its verdicts) never
//!   keeps another call from running.
//! - **Admitted calls.** Every admitted call holds one guest thread from the
//!   moment it is admitted until it returns, running or parked. Each
//!   [`CallKind`] has its own cap on admitted calls, so the guest threads a
//!   server ever needs are the sum of the caps (plus one per admitted call for
//!   a nested call, see [`Caps::threads`]) and a call never takes a token it
//!   has no thread for. The kinds are separate so one kind at its cap never
//!   holds back another: contest calls parked for their verdicts at the
//!   judging cap leave the work cap, where the evaluator calls that produce
//!   those verdicts run, untouched.
//!
//! Tokens are split between two classes. Each has a guaranteed share and may
//! borrow the other's idle tokens; a token freed while both classes wait goes
//! to the class below its share, and within it to a resuming call first.
//!
//! A queued waiter is an async future holding no thread. Dropping it leaves
//! the queue, so a request that went away no longer waits.
//!
//! Request path calls (the [`CallClass::Interactive`] kinds) are admitted per
//! plugin function, its *lane*: the calls of one function cost alike, while
//! calls of two functions can cost a thousand times apart. So:
//!
//! - One lane holds at most [`Capacity::lane_cap`] admitted calls, half the
//!   tokens: a slow function, however many calls it gets, leaves the other
//!   half to every other function.
//! - A call queues at most [`SHED_WAIT`], then is refused. A call whose lane
//!   is full is refused at once when the lane's own queue would take longer
//!   than that to drain, judged from how long that lane's calls have been
//!   taking. Either way the person gets a quick "busy, retry" instead of
//!   waiting out the request's deadline, and a cheap function is never
//!   refused for what a slow one costs.
//! - One plugin's routes together hold at most [`Capacity::plugin_cap`]
//!   admitted calls, every token but two, so a plugin with many slow routes
//!   still leaves a token to the host and one to another plugin.
//! - The host's own request path calls ([`CallKind::Request`]: visibility
//!   queries, request hooks), which the core API depends on, take a freed
//!   token ahead of plugin routes ([`CallKind::Route`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::oneshot;

/// Which share of the CPU tokens a call draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallClass {
    /// Work a person is waiting on: visibility queries, request hooks, plugin
    /// HTTP routes.
    Interactive,
    /// Work nobody is waiting on directly: judging, operations, timers,
    /// background hooks.
    Background,
}

impl CallClass {
    const ALL: [CallClass; 2] = [CallClass::Interactive, CallClass::Background];

    fn index(self) -> usize {
        match self {
            CallClass::Interactive => 0,
            CallClass::Background => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CallClass::Interactive => "interactive",
            CallClass::Background => "background",
        }
    }
}

/// What a top-level call is, which decides its cap on admitted calls and its
/// token class. Every top-level call site names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// A call the host makes on the request path: visibility queries and
    /// request hooks. The core API waits on these, so they take a freed
    /// token ahead of [`Route`] calls.
    ///
    /// [`Route`]: CallKind::Route
    Request,
    /// A plugin HTTP route.
    Route,
    /// A contest call that waits for its judgement (`on_submission`,
    /// `on_code_run`): parked most of its life on the calls of [`Work`].
    ///
    /// [`Work`]: CallKind::Work
    Judging,
    /// Every other background call: evaluators, detached callbacks, timers.
    /// Never waits on another top-level call.
    Work,
    /// A hook dispatched off the request path.
    Hook,
    /// A plugin's `init`: holds no token, so it never queues behind either
    /// class.
    Lifecycle,
}

/// How many [`CallKind`]s there are.
const KINDS: usize = 6;

impl CallKind {
    #[cfg(test)]
    const ALL: [CallKind; KINDS] = [
        CallKind::Request,
        CallKind::Route,
        CallKind::Judging,
        CallKind::Work,
        CallKind::Hook,
        CallKind::Lifecycle,
    ];

    fn index(self) -> usize {
        match self {
            CallKind::Request => 0,
            CallKind::Route => 1,
            CallKind::Judging => 2,
            CallKind::Work => 3,
            CallKind::Hook => 4,
            CallKind::Lifecycle => 5,
        }
    }

    /// The token class, or `None` for a call that runs without a token.
    pub fn class(self) -> Option<CallClass> {
        match self {
            CallKind::Request | CallKind::Route => Some(CallClass::Interactive),
            CallKind::Judging | CallKind::Work | CallKind::Hook => Some(CallClass::Background),
            CallKind::Lifecycle => None,
        }
    }

    /// Whether a person waits on calls of this kind: they are admitted per
    /// plugin function, queue at most [`SHED_WAIT`] and are shed when their
    /// function's queue would outlast it.
    fn is_request_path(self) -> bool {
        self.class() == Some(CallClass::Interactive)
    }

    /// Order within the class: a freed token goes to the waiting kind of the
    /// lowest rank, and within a rank to the oldest waiter.
    fn rank(self) -> u8 {
        match self {
            CallKind::Route => 1,
            CallKind::Request
            | CallKind::Judging
            | CallKind::Work
            | CallKind::Hook
            | CallKind::Lifecycle => 0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CallKind::Request => "request",
            CallKind::Route => "route",
            CallKind::Judging => "judging",
            CallKind::Work => "work",
            CallKind::Hook => "hook",
            CallKind::Lifecycle => "lifecycle",
        }
    }
}

/// The fewest tokens a host runs with: one per [`CallClass`].
pub const MIN_TOKENS: usize = 2;

/// Tokens a server runs guests with, per available core. Guest code is CPU
/// bound, so one token per core would saturate the cores only if no call ever
/// stalled while holding a token; calls do stall briefly (building an
/// instance, a page fault on a fresh linear memory, the hop between the async
/// waiter and its guest thread). Two per core keeps the cores busy through
/// those stalls while bounding the run queue to two guests per core.
pub const TOKENS_PER_CORE: usize = 2;

/// Admitted calls of each request path kind (running or parked) per CPU
/// token: room for request calls parked on a slow database without letting
/// them hold threads without bound.
pub const REQUESTS_PER_TOKEN: usize = 4;

/// Admitted contest calls before the server knows its in-flight cap.
pub const DEFAULT_JUDGING_CAP: usize = 256;

/// Lifecycle calls admitted at once: plugins load a few at a time.
pub const LIFECYCLE_CAP: usize = 4;

/// The longest a request path call queues before it is refused: a person
/// waiting on a page is better served by a retry a moment later than by a
/// minute behind slow calls.
pub const SHED_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Weight of the newest call's duration in its lane's running average.
const SERVICE_TIME_WEIGHT: f64 = 0.2;

/// The cap on admitted calls of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps([usize; KINDS]);

impl Caps {
    pub fn get(&self, kind: CallKind) -> usize {
        self.0[kind.index()]
    }

    /// Admitted top-level calls at most.
    pub fn total(&self) -> usize {
        self.0.iter().sum()
    }

    /// Guest threads the caps can ever need: one per admitted call, and one
    /// more for each one's nested call. Nesting stops at depth 2 and a
    /// parent is parked while its nested call runs, so an admitted call has
    /// at most one nested call at a time.
    pub fn threads(&self) -> usize {
        2 * self.total()
    }
}

/// The shape of a server's admission: how many tokens, how they split, and
/// the caps on admitted calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub tokens: usize,
    /// Guaranteed tokens per class, indexed like [`CallClass`]; they sum to
    /// `tokens`.
    shares: [usize; 2],
    pub caps: Caps,
}

impl Capacity {
    /// `tokens` CPU tokens split evenly between the classes (interactive gets
    /// the odd one), at least [`MIN_TOKENS`] so each class has one. The
    /// judging and work caps start from defaults until the host sets them
    /// from its in-flight cap and fan-out bound
    /// ([`Admission::set_background_caps`]).
    pub fn new(tokens: usize) -> Self {
        let tokens = tokens.max(MIN_TOKENS);
        let interactive = tokens.div_ceil(2);
        let mut caps = [0; KINDS];
        caps[CallKind::Request.index()] = tokens * REQUESTS_PER_TOKEN;
        caps[CallKind::Route.index()] = tokens * REQUESTS_PER_TOKEN;
        caps[CallKind::Judging.index()] = DEFAULT_JUDGING_CAP;
        caps[CallKind::Work.index()] = 2 * tokens;
        caps[CallKind::Hook.index()] = tokens;
        caps[CallKind::Lifecycle.index()] = LIFECYCLE_CAP;
        Self {
            tokens,
            shares: [interactive, tokens - interactive],
            caps: Caps(caps),
        }
    }

    /// `max_concurrent_calls` tokens when configured (at least
    /// [`MIN_TOKENS`]), else [`Self::for_this_host`]. The executor's admission
    /// and the server's plugin database pools are both sized from this.
    pub fn for_config(max_concurrent_calls: Option<usize>) -> Self {
        max_concurrent_calls.map_or_else(Self::for_this_host, Self::new)
    }

    /// [`TOKENS_PER_CORE`] tokens per core this process may use.
    pub fn for_this_host() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self::new(cores * TOKENS_PER_CORE)
    }

    pub fn share(&self, class: CallClass) -> usize {
        self.shares[class.index()]
    }

    /// Admitted request path calls of one plugin function at most: half the
    /// tokens (at least one), so one function never holds more than half of
    /// them and the rest stay free for every other function. A function's
    /// calls beyond it wait on that function's own calls only.
    pub fn lane_cap(&self) -> usize {
        (self.tokens / 2).max(1)
    }

    /// Admitted [`CallKind::Route`] calls of one plugin at most, over all its
    /// routes: every token but two (at least one). However many slow routes
    /// one plugin has, one token stays for the host's [`CallKind::Request`]
    /// calls and one for another plugin's routes. With two tokens a plugin's
    /// routes hold at most one and the other is shared, the host's calls
    /// taking it first.
    pub fn plugin_cap(&self) -> usize {
        self.tokens.saturating_sub(2).max(1)
    }

    /// The same capacity with `kind` capped at `cap` (at least 1).
    pub fn with_cap(mut self, kind: CallKind, cap: usize) -> Self {
        self.caps.0[kind.index()] = cap.max(1);
        self
    }
}

/// What a granted waiter receives: its token, or none for a lifecycle call.
type Grant = Option<Token>;

/// The lane a call queues in and is counted under: its plugin function for a
/// request path kind, its kind alone for every other kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LaneKey {
    kind: CallKind,
    /// `(plugin, function)`, for a request path kind only.
    function: Option<Arc<(Box<str>, Box<str>)>>,
}

impl LaneKey {
    fn new(kind: CallKind, plugin: &str, function: &str) -> Self {
        Self {
            kind,
            function: kind
                .is_request_path()
                .then(|| Arc::new((plugin.into(), function.into()))),
        }
    }

    /// The plugin whose route this lane is, for the plugin cap.
    fn route_plugin(&self) -> Option<&str> {
        match (self.kind, &self.function) {
            (CallKind::Route, Some(function)) => Some(&function.0),
            _ => None,
        }
    }

    /// The lane's cap on admitted calls; its kind's cap applies besides.
    fn cap(&self, capacity: &Capacity) -> usize {
        match self.function {
            Some(_) => capacity.lane_cap(),
            None => usize::MAX,
        }
    }
}

/// The calls of one [`LaneKey`].
#[derive(Default)]
struct Lane {
    admitted: usize,
    /// Queued calls, oldest first.
    queued: BTreeMap<u64, oneshot::Sender<Grant>>,
    /// Running average of how long the lane's calls took, admitted to done;
    /// `None` until one finished. Kept for request path lanes only.
    secs: Option<f64>,
}

impl Lane {
    /// How long a call joining this full lane's queue now is expected to
    /// wait: the calls queued ahead of it and itself, served by the lane's
    /// admitted calls at their average duration. `None` before any finished.
    fn expected_wait(&self) -> Option<f64> {
        let per_call = self.secs?;
        let ahead = self.queued.len() + 1;
        Some(ahead as f64 * per_call / self.admitted.max(1) as f64)
    }

    fn observe(&mut self, secs: f64) {
        self.secs = Some(match self.secs {
            Some(avg) => avg + SERVICE_TIME_WEIGHT * (secs - avg),
            None => secs,
        });
    }
}

struct State {
    capacity: Capacity,
    running: [usize; 2],
    /// Admitted calls per kind, for the kind caps.
    admitted: [usize; KINDS],
    next_waiter: u64,
    /// Every lane a call was ever admitted to or queued in. Request path
    /// lanes are the plugin functions manifests declare, so there are
    /// boundedly many; an idle lane keeps its running average.
    lanes: HashMap<LaneKey, Lane>,
    /// Admitted route calls per plugin with any, for the plugin cap.
    routes: HashMap<Box<str>, usize>,
    /// Parked calls returning from a host function, per class. Within the
    /// class a token goes to, they go ahead of its queued calls.
    resuming: [VecDeque<SyncSender<Token>>; 2],
}

impl State {
    fn running_total(&self) -> usize {
        self.running.iter().sum()
    }

    /// Whether a call of `key` may be admitted: its kind, its lane and, for
    /// a route, its plugin are all under their caps.
    fn has_room(&self, key: &LaneKey, lane: &Lane) -> bool {
        self.admitted[key.kind.index()] < self.capacity.caps.get(key.kind)
            && lane.admitted < key.cap(&self.capacity)
            && key.route_plugin().is_none_or(|plugin| {
                self.routes.get(plugin).copied().unwrap_or(0) < self.capacity.plugin_cap()
            })
    }

    /// The lane of `class` whose queued call goes next: among lanes with
    /// room, the lowest kind rank, then the oldest call.
    fn next_lane(&self, class: CallClass) -> Option<LaneKey> {
        self.lanes
            .iter()
            .filter(|(key, lane)| key.kind.class() == Some(class) && self.has_room(key, lane))
            .filter_map(|(key, lane)| {
                let oldest = *lane.queued.first_key_value()?.0;
                Some(((key.kind.rank(), oldest), key))
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, key)| key.clone())
    }

    /// The class the next free token goes to, if any waiter of it may take
    /// it: a class below its share first, so a class whose calls keep
    /// parking and resuming cannot hold every token.
    fn next_class(&self) -> Option<CallClass> {
        let waiting =
            |c: CallClass| !self.resuming[c.index()].is_empty() || self.next_lane(c).is_some();
        let below_share = |c: CallClass| self.running[c.index()] < self.capacity.share(c);
        CallClass::ALL
            .into_iter()
            .find(|&c| waiting(c) && below_share(c))
            .or_else(|| CallClass::ALL.into_iter().find(|&c| waiting(c)))
    }

    /// Count a call of `key` as admitted.
    fn enter(&mut self, key: &LaneKey) {
        self.admitted[key.kind.index()] += 1;
        self.lanes.entry(key.clone()).or_default().admitted += 1;
        if let Some(plugin) = key.route_plugin() {
            *self.routes.entry(plugin.into()).or_default() += 1;
        }
    }

    /// Count a call of `key` as no longer admitted; returns its lane.
    fn leave(&mut self, key: &LaneKey) -> &mut Lane {
        self.admitted[key.kind.index()] -= 1;
        if let Some(plugin) = key.route_plugin()
            && let Some(admitted) = self.routes.get_mut(plugin)
        {
            *admitted -= 1;
            if *admitted == 0 {
                self.routes.remove(plugin);
            }
        }
        let lane = self
            .lanes
            .get_mut(key)
            .expect("an admitted call's lane exists");
        lane.admitted -= 1;
        lane
    }
}

/// The server-wide admission. Cheap to clone; clones share one state.
#[derive(Clone)]
pub struct Admission(Arc<Mutex<State>>);

/// One CPU token, held while guest code runs. Dropping it frees the token.
#[must_use = "dropping a token frees it"]
pub struct Token {
    admission: Admission,
    class: CallClass,
    armed: bool,
}

/// An admitted top-level call, held for the whole call: its place under its
/// kind's and its lane's caps, and so its guest thread.
#[must_use = "dropping the admission ends the call"]
pub struct Admitted {
    admission: Admission,
    lane: LaneKey,
    queued: bool,
    /// On the runtime's clock, which tests drive.
    since: tokio::time::Instant,
}

/// A top-level call that could not be admitted in time: it reached its
/// deadline while queued, or, for a request path call, it queued for
/// [`SHED_WAIT`] or its lane's queue was expected to take longer than that.
#[derive(Debug, PartialEq, Eq)]
pub struct Expired;

impl Admission {
    pub fn new(capacity: Capacity) -> Self {
        Self(Arc::new(Mutex::new(State {
            capacity,
            running: [0; 2],
            admitted: [0; KINDS],
            next_waiter: 0,
            lanes: HashMap::new(),
            routes: HashMap::new(),
            resuming: [VecDeque::new(), VecDeque::new()],
        })))
    }

    pub fn capacity(&self) -> Capacity {
        self.0.lock().capacity
    }

    /// Set the judging cap to the server's in-flight cap and the work cap to
    /// its fan-out bound plus one per token (room for callbacks and timers
    /// beside the evaluators). Raising a cap admits queued calls at once;
    /// lowering one lets the admitted calls above it finish.
    pub fn set_background_caps(&self, in_flight: usize, fan_out: usize) {
        let mut state = self.0.lock();
        let tokens = state.capacity.tokens;
        state.capacity = state
            .capacity
            .with_cap(CallKind::Judging, in_flight)
            .with_cap(CallKind::Work, fan_out + tokens);
        self.grant(&mut state);
    }

    /// Guests running now, per class.
    pub fn running(&self, class: CallClass) -> usize {
        self.0.lock().running[class.index()]
    }

    /// Top-level calls queued now, per class.
    pub fn queued(&self, class: CallClass) -> usize {
        self.queued_where(|kind| kind.class() == Some(class))
    }

    /// Top-level calls of `kind` queued now.
    #[cfg(test)]
    pub(crate) fn queued_kind(&self, kind: CallKind) -> usize {
        self.queued_where(|k| k == kind)
    }

    fn queued_where(&self, matches: impl Fn(CallKind) -> bool) -> usize {
        self.0
            .lock()
            .lanes
            .iter()
            .filter(|(key, _)| matches(key.kind))
            .map(|(_, lane)| lane.queued.len())
            .sum()
    }

    /// Calls of `kind` admitted now.
    pub fn admitted(&self, kind: CallKind) -> usize {
        self.0.lock().admitted[kind.index()]
    }

    /// Admit a top-level call of `kind` to `function` of `plugin` and give it
    /// a token (none for a lifecycle call), queueing until its kind and lane
    /// have room and a token is free. Gives up at `deadline`, and a request
    /// path call after [`SHED_WAIT`] in the queue. Cancel-safe: dropping the
    /// future leaves the queue.
    pub async fn admit(
        &self,
        kind: CallKind,
        plugin: &str,
        function: &str,
        deadline: Instant,
    ) -> Result<(Admitted, Option<Token>), Expired> {
        let key = LaneKey::new(kind, plugin, function);
        let (id, rx) = {
            let mut guard = self.0.lock();
            let state = &mut *guard;
            let cap = key.cap(&state.capacity);
            let lane = state.lanes.entry(key.clone()).or_default();
            // A call that waits on its own lane's calls, and would wait too
            // long for them, is refused unqueued.
            let shed = lane.admitted >= cap
                && lane
                    .expected_wait()
                    .is_some_and(|wait| wait > SHED_WAIT.as_secs_f64());
            let token_free = state.running_total() < state.capacity.tokens;
            if state.has_room(&key, &state.lanes[&key]) && (kind.class().is_none() || token_free) {
                state.enter(&key);
                let token = kind.class().map(|class| {
                    state.running[class.index()] += 1;
                    self.token(class)
                });
                drop(guard);
                return Ok((self.admission(key, false), token));
            }
            if shed {
                return Err(Expired);
            }
            let (tx, rx) = oneshot::channel();
            let id = state.next_waiter;
            state.next_waiter += 1;
            if let Some(lane) = state.lanes.get_mut(&key) {
                lane.queued.insert(id, tx);
            }
            (id, rx)
        };
        let mut give_up = tokio::time::Instant::from_std(deadline);
        if kind.is_request_path() {
            give_up = give_up.min(tokio::time::Instant::now() + SHED_WAIT);
        }
        let mut waiting = Waiting {
            admission: self,
            lane: key,
            id,
            rx,
            granted: false,
        };
        match tokio::time::timeout_at(give_up, &mut waiting.rx).await {
            Ok(Ok(token)) => {
                // The grant already counted this call as admitted.
                waiting.granted = true;
                Ok((self.admission(waiting.lane.clone(), true), token))
            }
            // The sender is only dropped after a failed delivery, which
            // cannot happen while `waiting.rx` is alive.
            Ok(Err(_)) | Err(_) => Err(Expired),
        }
    }

    /// Take a token for a parked call of `class` returning to guest code,
    /// ahead of its class's queued calls. Blocks the calling thread; gives up
    /// at `deadline`, after which the guest is cancelled anyway.
    pub fn resume(&self, class: CallClass, deadline: Instant) -> Option<Token> {
        let rx: Receiver<Token> = {
            let mut state = self.0.lock();
            if state.running_total() < state.capacity.tokens {
                state.running[class.index()] += 1;
                drop(state);
                return Some(self.token(class));
            }
            let (tx, rx) = sync_channel(1);
            state.resuming[class.index()].push_back(tx);
            rx
        };
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    }

    fn token(&self, class: CallClass) -> Token {
        Token {
            admission: self.clone(),
            class,
            armed: true,
        }
    }

    fn admission(&self, lane: LaneKey, queued: bool) -> Admitted {
        Admitted {
            admission: self.clone(),
            lane,
            queued,
            since: tokio::time::Instant::now(),
        }
    }

    /// Hand out what freed up. Lifecycle calls need only room under their
    /// cap. A token goes to the class by share, then within it to a resuming
    /// call before the next queued call whose kind and lane have room.
    /// Called with the lock held after a token or an admission frees up.
    fn grant(&self, state: &mut State) {
        let lifecycle = LaneKey::new(CallKind::Lifecycle, "", "");
        while state.admitted[CallKind::Lifecycle.index()]
            < state.capacity.caps.get(CallKind::Lifecycle)
        {
            let Some((_, tx)) = state
                .lanes
                .get_mut(&lifecycle)
                .and_then(|lane| lane.queued.pop_first())
            else {
                break;
            };
            state.enter(&lifecycle);
            if tx.send(None).is_err() {
                state.leave(&lifecycle);
            }
        }
        while state.running_total() < state.capacity.tokens {
            let Some(class) = state.next_class() else {
                return;
            };
            let c = class.index();
            if let Some(tx) = state.resuming[c].pop_front() {
                state.running[c] += 1;
                if let Err(TrySendError::Full(mut token) | TrySendError::Disconnected(mut token)) =
                    tx.try_send(self.token(class))
                {
                    // The resumer stopped waiting; the token stays free.
                    token.armed = false;
                    state.running[c] -= 1;
                }
                continue;
            }
            let Some(key) = state.next_lane(class) else {
                return;
            };
            let Some((_, tx)) = state
                .lanes
                .get_mut(&key)
                .and_then(|lane| lane.queued.pop_first())
            else {
                return;
            };
            state.running[c] += 1;
            state.enter(&key);
            if let Err(Some(mut token)) = tx.send(Some(self.token(class))) {
                token.armed = false;
                state.running[c] -= 1;
                state.leave(&key);
            }
        }
    }
}

/// A queued top-level call. Dropped before it was granted (its deadline
/// passed or its caller went away), it leaves the queue; if a grant raced the
/// drop, it hands the granted token and admission back.
struct Waiting<'a> {
    admission: &'a Admission,
    lane: LaneKey,
    id: u64,
    rx: oneshot::Receiver<Grant>,
    granted: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.granted {
            return;
        }
        let raced = {
            let mut state = self.admission.0.lock();
            let still_queued = state
                .lanes
                .get_mut(&self.lane)
                .is_some_and(|lane| lane.queued.remove(&self.id).is_some());
            if still_queued {
                None
            } else {
                // Granted under the lock before we took it: the grant is in
                // the channel and the call was counted as admitted.
                state.leave(&self.lane);
                self.admission.grant(&mut state);
                self.rx.try_recv().ok().flatten()
            }
        };
        // Released outside the lock: the token's drop takes it again.
        drop(raced);
    }
}

impl Admitted {
    /// Whether the call had to queue.
    pub fn queued(&self) -> bool {
        self.queued
    }

    pub fn kind(&self) -> CallKind {
        self.lane.kind
    }
}

impl Token {
    pub fn class(&self) -> CallClass {
        self.class
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.admission.0.lock();
        state.running[self.class.index()] -= 1;
        self.admission.grant(&mut state);
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut state = self.admission.0.lock();
        let lane = state.leave(&self.lane);
        if self.lane.function.is_some() {
            lane.observe(self.since.elapsed().as_secs_f64());
        }
        self.admission.grant(&mut state);
    }
}

#[cfg(test)]
mod tests {
    //! Every test runs on a paused clock: time moves only when a test
    //! advances it, or when the runtime has nothing left to run.
    use super::*;
    use std::time::Duration;

    const INTERACTIVE: CallKind = CallKind::Request;
    const BACKGROUND: CallKind = CallKind::Work;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    type Admit = (Admitted, Option<Token>);

    fn try_admit(a: &Admission, kind: CallKind, function: &str) -> Option<Result<Admit, Expired>> {
        futures::FutureExt::now_or_never(a.admit(kind, "p", function, far()))
    }

    async fn admit_to(a: &Admission, kind: CallKind, function: &str) -> Admit {
        a.admit(kind, "p", function, far()).await.expect("admitted")
    }

    async fn admit(a: &Admission, kind: CallKind) -> Admit {
        admit_to(a, kind, "f").await
    }

    /// Queue an admission to `function` on its own task; returns its handle
    /// once queued.
    async fn queue_to(
        a: &Admission,
        kind: CallKind,
        function: &str,
    ) -> tokio::task::JoinHandle<Result<Admit, Expired>> {
        let before = a.queued_kind(kind);
        let task = tokio::spawn({
            let (a, function) = (a.clone(), function.to_owned());
            async move { a.admit(kind, "p", &function, far()).await }
        });
        while a.queued_kind(kind) == before {
            tokio::task::yield_now().await;
        }
        task
    }

    async fn queue(
        a: &Admission,
        kind: CallKind,
    ) -> tokio::task::JoinHandle<Result<Admit, Expired>> {
        queue_to(a, kind, "f").await
    }

    /// A capacity of `tokens` whose kind caps never bind.
    fn roomy(tokens: usize) -> Capacity {
        CallKind::ALL
            .into_iter()
            .fold(Capacity::new(tokens), |c, k| c.with_cap(k, 1000))
    }

    /// Calls of `function` that took `secs` each, `n` times, one at a time.
    async fn served(a: &Admission, kind: CallKind, function: &str, secs: f64, n: usize) {
        for _ in 0..n {
            let call = admit_to(a, kind, function).await;
            tokio::time::advance(Duration::from_secs_f64(secs)).await;
            drop(call);
        }
    }

    fn lane_secs(a: &Admission, kind: CallKind, function: &str) -> Option<f64> {
        a.0.lock()
            .lanes
            .get(&LaneKey::new(kind, "p", function))
            .and_then(|lane| lane.secs)
    }

    #[tokio::test(start_paused = true)]
    async fn cheap_calls_are_served_while_a_slow_route_floods_its_lane() {
        let a = Admission::new(Capacity::new(8));
        // A route that takes 5 s a call, called twice in turn.
        served(&a, CallKind::Route, "burn", 5.0, 2).await;
        // Then twelve of its calls at once: it gets half the tokens, queues
        // what its own calls drain within the shed wait, sheds the rest.
        let mut burning = Vec::new();
        let mut queued = Vec::new();
        let mut shed = 0;
        for _ in 0..12 {
            match try_admit(&a, CallKind::Route, "burn") {
                Some(Ok(call)) => burning.push(call),
                Some(Err(Expired)) => shed += 1,
                None => queued.push(queue_to(&a, CallKind::Route, "burn").await),
            }
        }
        assert_eq!(
            (burning.len(), queued.len(), shed),
            (a.capacity().lane_cap(), 1, 7)
        );
        // A visibility query, another route of the same plugin: both run now.
        let visibility = try_admit(&a, CallKind::Request, "query");
        assert!(matches!(visibility, Some(Ok((_, Some(_))))));
        let reflect = try_admit(&a, CallKind::Route, "reflect");
        assert!(matches!(reflect, Some(Ok((_, Some(_))))));
        // The queued slow call is refused once it waited the shed wait.
        tokio::time::advance(SHED_WAIT).await;
        for call in queued {
            assert_eq!(call.await.unwrap().err(), Some(Expired));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_burst_is_served_or_refused_within_the_shed_wait() {
        let a = Admission::new(Capacity::new(4));
        // The lane's calls have been cheap, so a burst of slow ones queues.
        served(&a, INTERACTIVE, "f", 0.01, 5).await;
        let _slow = [admit(&a, INTERACTIVE).await, admit(&a, INTERACTIVE).await];
        let started = tokio::time::Instant::now();
        let mut burst = Vec::new();
        for _ in 0..6 {
            burst.push(queue(&a, INTERACTIVE).await);
        }
        let _busy = [admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await];
        let background = queue(&a, BACKGROUND).await;
        // Its calls run past the shed wait: every queued call is refused at
        // it, none waits for the request's deadline.
        tokio::time::advance(SHED_WAIT).await;
        for call in burst {
            assert_eq!(call.await.unwrap().err(), Some(Expired));
        }
        assert_eq!(started.elapsed(), SHED_WAIT);
        assert_eq!(a.queued(CallClass::Interactive), 0);
        // Nobody waits on a background call: it keeps its place.
        assert!(!background.is_finished());
        assert_eq!(a.queued(CallClass::Background), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_plugin_flooding_several_slow_routes_leaves_tokens_to_others() {
        let a = Admission::new(Capacity::new(8));
        let route = |plugin: &str, function: &str| {
            futures::FutureExt::now_or_never(a.admit(CallKind::Route, plugin, function, far()))
        };
        // Three slow routes of one plugin, each flooded: the first fills its
        // lane, the second runs up to the plugin's cap, the rest queue.
        let mut running = Vec::new();
        let mut queued = Vec::new();
        for function in ["s1", "s2", "s3"] {
            for _ in 0..6 {
                match route("slow", function) {
                    Some(Ok(call)) => running.push(call),
                    Some(Err(Expired)) => panic!("nothing finished, so nothing is shed"),
                    None => queued.push(tokio::spawn({
                        let (a, function) = (a.clone(), function.to_owned());
                        async move { a.admit(CallKind::Route, "slow", &function, far()).await }
                    })),
                }
            }
        }
        while a.queued_kind(CallKind::Route) < queued.len() {
            tokio::task::yield_now().await;
        }
        assert_eq!(running.len(), a.capacity().plugin_cap());
        assert_eq!(a.running(CallClass::Interactive), 6);
        // Another plugin's cheap route and a host visibility query both run.
        assert!(matches!(route("other", "page"), Some(Ok((_, Some(_))))));
        let visibility = a.admit(CallKind::Request, "slow", "query", far());
        assert!(matches!(
            futures::FutureExt::now_or_never(visibility),
            Some(Ok((_, Some(_))))
        ));
        // The flood's queued calls are refused once they waited the shed wait.
        tokio::time::advance(SHED_WAIT).await;
        for call in queued {
            assert_eq!(call.await.unwrap().err(), Some(Expired));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_lane_holds_its_calls_while_other_lanes_run() {
        let a = Admission::new(roomy(4));
        let mut slow = vec![admit(&a, INTERACTIVE).await, admit(&a, INTERACTIVE).await];
        // Tokens are free, but the lane holds half of them already.
        let waiting = queue(&a, INTERACTIVE).await;
        assert_eq!(a.running(CallClass::Interactive), 2);
        let _other = try_admit(&a, INTERACTIVE, "g")
            .expect("ran at once")
            .unwrap();
        slow.pop();
        let _granted = waiting.await.unwrap().unwrap();
        assert_eq!(a.admitted(INTERACTIVE), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn host_request_calls_take_a_freed_token_ahead_of_plugin_routes() {
        let a = Admission::new(roomy(2));
        let mut held = vec![admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await];
        let route = queue_to(&a, CallKind::Route, "page").await;
        let request = queue_to(&a, CallKind::Request, "query").await;
        held.pop();
        let _granted = request.await.unwrap().unwrap();
        assert!(!route.is_finished());
        held.pop();
        let _granted = route.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_to_a_full_lane_is_refused_at_once_when_its_queue_would_outlast_the_shed_wait() {
        let a = Admission::new(roomy(4));
        // Calls of 0.5 s on two admitted calls: one queued ahead is ~0.5 s.
        served(&a, INTERACTIVE, "f", 0.5, 1).await;
        let _held = [admit(&a, INTERACTIVE).await, admit(&a, INTERACTIVE).await];
        let _ahead = queue(&a, INTERACTIVE).await;
        assert!(try_admit(&a, INTERACTIVE, "f").is_none(), "queues");

        // Calls of 10 s: the wait would be far past it, so refused unqueued.
        a.0.lock()
            .lanes
            .get_mut(&LaneKey::new(INTERACTIVE, "p", "f"))
            .unwrap()
            .secs = Some(10.0);
        assert!(matches!(
            try_admit(&a, INTERACTIVE, "f"),
            Some(Err(Expired))
        ));
        assert_eq!(a.queued_kind(INTERACTIVE), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lane_with_no_finished_call_refuses_nothing_unqueued() {
        let a = Admission::new(roomy(2));
        let _held = admit(&a, INTERACTIVE).await;
        assert!(try_admit(&a, INTERACTIVE, "f").is_none(), "queues");
    }

    #[tokio::test(start_paused = true)]
    async fn the_service_time_follows_the_finished_calls_of_its_own_lane() {
        let a = Admission::new(roomy(2));
        served(&a, INTERACTIVE, "f", 1.0, 1).await;
        served(&a, INTERACTIVE, "f", 2.0, 1).await;
        served(&a, INTERACTIVE, "g", 0.001, 1).await;
        assert!((lane_secs(&a, INTERACTIVE, "f").unwrap() - 1.2).abs() < 1e-9);
        assert!((lane_secs(&a, INTERACTIVE, "g").unwrap() - 0.001).abs() < 1e-9);
        // The same function as another kind is another lane.
        assert_eq!(lane_secs(&a, CallKind::Route, "f"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn never_more_tokens_out_than_capacity() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, BACKGROUND).await);
        }
        let waiter = queue(&a, BACKGROUND).await;
        assert_eq!(a.running(CallClass::Background), 4);
        held.pop();
        let _granted = waiter.await.unwrap().unwrap();
        assert_eq!(a.running(CallClass::Background), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_class_borrows_the_other_classes_idle_tokens() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, BACKGROUND).await);
        }
        assert_eq!(a.running(CallClass::Background), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn interactive_gets_the_next_token_under_a_background_flood() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, BACKGROUND).await);
        }
        let background = queue(&a, BACKGROUND).await;
        let interactive = queue(&a, INTERACTIVE).await;
        held.pop();
        let _granted = interactive.await.unwrap().unwrap();
        assert_eq!(a.queued(CallClass::Background), 1);
        assert!(!background.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn background_gets_the_next_token_under_an_interactive_flood() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for f in ["f", "g", "h", "i"] {
            held.push(admit_to(&a, INTERACTIVE, f).await);
        }
        let interactive = queue_to(&a, INTERACTIVE, "j").await;
        let background = queue(&a, BACKGROUND).await;
        held.pop();
        let _granted = background.await.unwrap().unwrap();
        assert_eq!(a.queued(CallClass::Interactive), 1);
        assert!(!interactive.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn a_class_at_its_share_yields_to_the_other_but_not_to_itself() {
        let a = Admission::new(roomy(4));
        let mut interactive = Vec::new();
        for f in ["f", "g"] {
            interactive.push(admit_to(&a, INTERACTIVE, f).await);
        }
        let mut background = Vec::new();
        for _ in 0..2 {
            background.push(admit(&a, BACKGROUND).await);
        }
        let more_interactive = queue_to(&a, INTERACTIVE, "h").await;
        let more_background = queue(&a, BACKGROUND).await;
        // Background frees one: background is now below its share, so the
        // token stays with background.
        background.pop();
        let _granted = more_background.await.unwrap().unwrap();
        assert!(!more_interactive.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_waiter_leaves_the_queue() {
        let a = Admission::new(roomy(2));
        let _held = (admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await);
        let waiter = queue(&a, INTERACTIVE).await;
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_past_its_deadline_expires() {
        let a = Admission::new(roomy(2));
        let _held = (admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await);
        let expired = a
            .admit(INTERACTIVE, "p", "f", Instant::now())
            .await
            .err()
            .expect("no token was free before the deadline");
        assert_eq!(expired, Expired);
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resuming_call_goes_ahead_of_its_class_queue() {
        let a = Admission::new(roomy(2));
        let (admitted, parked_token) = admit(&a, BACKGROUND).await;
        let other = admit(&a, BACKGROUND).await;
        let queued = queue(&a, BACKGROUND).await;
        // The first call parks: its token goes to the queued call.
        drop(parked_token);
        let granted = queued.await.unwrap().unwrap();
        // Now it wants back in while another call of its class queues.
        let late = queue(&a, BACKGROUND).await;
        let resumer = resumer(&a, CallClass::Background);
        drop(other);
        assert_eq!(resumer.join().unwrap(), Some(CallClass::Background));
        assert!(!late.is_finished());
        drop((admitted, granted));
    }

    #[tokio::test(start_paused = true)]
    async fn a_kind_at_its_cap_holds_parked_calls_and_not_other_kinds() {
        let a = Admission::new(roomy(2).with_cap(CallKind::Judging, 3));
        // Three contest calls admitted and parked: the judging cap is full
        // though every token is free.
        let mut parked = Vec::new();
        for _ in 0..3 {
            let (admitted, token) = admit(&a, CallKind::Judging).await;
            drop(token);
            parked.push(admitted);
        }
        let fourth = queue(&a, CallKind::Judging).await;
        assert_eq!(a.running(CallClass::Background), 0);
        // The evaluator calls they wait on still get in.
        let _work = admit(&a, CallKind::Work).await;
        parked.pop();
        let _granted = fourth.await.unwrap().unwrap();
        assert_eq!(a.admitted(CallKind::Judging), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_kind_at_its_cap_does_not_block_later_kinds_of_its_class() {
        let a = Admission::new(roomy(2).with_cap(CallKind::Judging, 1));
        let held = admit(&a, BACKGROUND).await;
        let (judging, judging_token) = admit(&a, CallKind::Judging).await;
        let blocked = queue(&a, CallKind::Judging).await;
        let work = queue(&a, CallKind::Work).await;
        // A token frees; the oldest background waiter has no room, so the
        // work call behind it takes the token.
        drop(judging_token);
        let _granted = work.await.unwrap().unwrap();
        assert!(!blocked.is_finished());
        drop((judging, held));
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_calls_need_room_not_tokens() {
        let a = Admission::new(roomy(2).with_cap(CallKind::Lifecycle, 1));
        let _held = (admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await);
        let (first, token) = admit(&a, CallKind::Lifecycle).await;
        assert!(token.is_none(), "every token is taken; init still runs");
        let second = queue(&a, CallKind::Lifecycle).await;
        drop(first);
        let (_, token) = second.await.unwrap().unwrap();
        assert!(token.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn raising_a_cap_admits_queued_calls() {
        let a = Admission::new(roomy(4).with_cap(CallKind::Judging, 1));
        let _first = admit(&a, CallKind::Judging).await;
        let second = queue(&a, CallKind::Judging).await;
        a.set_background_caps(2, 8);
        let _granted = second.await.unwrap().unwrap();
        assert_eq!(a.capacity().caps.get(CallKind::Work), 8 + 4);
    }

    #[test]
    fn the_thread_budget_covers_every_cap_and_a_nested_call_each() {
        let caps = Capacity::new(8).caps;
        assert_eq!(
            caps.total(),
            2 * 32 + DEFAULT_JUDGING_CAP + 16 + 8 + LIFECYCLE_CAP
        );
        assert_eq!(caps.threads(), 2 * caps.total());
    }

    #[test]
    fn one_lane_holds_at_most_half_the_tokens() {
        assert_eq!(Capacity::new(8).lane_cap(), 4);
        assert_eq!(Capacity::new(3).lane_cap(), 1);
        assert_eq!(Capacity::new(MIN_TOKENS).lane_cap(), 1);
    }

    #[test]
    fn one_plugins_routes_leave_two_tokens_when_there_are_three_or_more() {
        assert_eq!(Capacity::new(8).plugin_cap(), 6);
        assert_eq!(Capacity::new(3).plugin_cap(), 1);
        assert_eq!(Capacity::new(MIN_TOKENS).plugin_cap(), 1);
    }

    /// Park a resumer of `class` on its own thread; returns once it waits.
    fn resumer(a: &Admission, class: CallClass) -> std::thread::JoinHandle<Option<CallClass>> {
        let before = a.0.lock().resuming[class.index()].len();
        let handle = std::thread::spawn({
            let a = a.clone();
            move || a.resume(class, far()).map(|t| t.class())
        });
        while a.0.lock().resuming[class.index()].len() == before {
            std::thread::yield_now();
        }
        handle
    }

    #[tokio::test(start_paused = true)]
    async fn interactive_gets_a_token_while_background_resumers_keep_arriving() {
        let a = Admission::new(roomy(4));
        // Background holds every token through calls that park and resume.
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, BACKGROUND).await);
        }
        let mut resumers = vec![resumer(&a, CallClass::Background)];
        let interactive = queue(&a, INTERACTIVE).await;
        // A background token frees; another background resumer arrives
        // before the next one does. Interactive, below its share, gets the
        // first; the resumers get what is left.
        held.pop();
        resumers.push(resumer(&a, CallClass::Background));
        let _granted = interactive.await.unwrap().unwrap();
        assert_eq!(a.running(CallClass::Interactive), 1);
        assert_eq!(a.0.lock().resuming[CallClass::Background.index()].len(), 2);
        drop(held);
        for r in resumers {
            assert_eq!(r.join().unwrap(), Some(CallClass::Background));
        }
    }
}
