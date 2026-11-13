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

use std::collections::{BTreeMap, VecDeque};
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

    fn kinds(self) -> &'static [CallKind] {
        match self {
            CallClass::Interactive => &[CallKind::Request],
            CallClass::Background => &[CallKind::Judging, CallKind::Work, CallKind::Hook],
        }
    }
}

/// What a top-level call is, which decides its cap on admitted calls and its
/// token class. Every top-level call site names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// A request path call: visibility, request hooks, plugin routes.
    Request,
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

impl CallKind {
    #[cfg(test)]
    const ALL: [CallKind; 5] = [
        CallKind::Request,
        CallKind::Judging,
        CallKind::Work,
        CallKind::Hook,
        CallKind::Lifecycle,
    ];

    fn index(self) -> usize {
        match self {
            CallKind::Request => 0,
            CallKind::Judging => 1,
            CallKind::Work => 2,
            CallKind::Hook => 3,
            CallKind::Lifecycle => 4,
        }
    }

    /// The token class, or `None` for a call that runs without a token.
    pub fn class(self) -> Option<CallClass> {
        match self {
            CallKind::Request => Some(CallClass::Interactive),
            CallKind::Judging | CallKind::Work | CallKind::Hook => Some(CallClass::Background),
            CallKind::Lifecycle => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CallKind::Request => "request",
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

/// Admitted request calls (running or parked) per CPU token: room for
/// request calls parked on a slow database without letting them hold
/// threads without bound.
pub const REQUESTS_PER_TOKEN: usize = 8;

/// Admitted contest calls before the server knows its in-flight cap.
pub const DEFAULT_JUDGING_CAP: usize = 256;

/// Lifecycle calls admitted at once: plugins load a few at a time.
pub const LIFECYCLE_CAP: usize = 4;

/// The cap on admitted calls of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps([usize; 5]);

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
        let mut caps = [0; 5];
        caps[CallKind::Request.index()] = tokens * REQUESTS_PER_TOKEN;
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

    /// The same capacity with `kind` capped at `cap` (at least 1).
    pub fn with_cap(mut self, kind: CallKind, cap: usize) -> Self {
        self.caps.0[kind.index()] = cap.max(1);
        self
    }
}

/// What a granted waiter receives: its token, or none for a lifecycle call.
type Grant = Option<Token>;

struct State {
    capacity: Capacity,
    running: [usize; 2],
    admitted: [usize; 5],
    next_waiter: u64,
    /// Queued top-level calls per kind, oldest first.
    queued: [BTreeMap<u64, oneshot::Sender<Grant>>; 5],
    /// Parked calls returning from a host function, per class. Within the
    /// class a token goes to, they go ahead of its queued calls.
    resuming: [VecDeque<SyncSender<Token>>; 2],
}

impl State {
    fn running_total(&self) -> usize {
        self.running.iter().sum()
    }

    fn has_room(&self, kind: CallKind) -> bool {
        self.admitted[kind.index()] < self.capacity.caps.get(kind)
    }

    /// The oldest queued call of `class` whose kind has room.
    fn next_kind(&self, class: CallClass) -> Option<CallKind> {
        class
            .kinds()
            .iter()
            .copied()
            .filter(|&k| self.has_room(k))
            .filter_map(|k| Some((*self.queued[k.index()].first_key_value()?.0, k)))
            .min_by_key(|(id, _)| *id)
            .map(|(_, k)| k)
    }

    /// The class the next free token goes to, if any waiter of it may take
    /// it: a class below its share first, so a class whose calls keep
    /// parking and resuming cannot hold every token.
    fn next_class(&self) -> Option<CallClass> {
        let waiting =
            |c: CallClass| !self.resuming[c.index()].is_empty() || self.next_kind(c).is_some();
        let below_share = |c: CallClass| self.running[c.index()] < self.capacity.share(c);
        CallClass::ALL
            .into_iter()
            .find(|&c| waiting(c) && below_share(c))
            .or_else(|| CallClass::ALL.into_iter().find(|&c| waiting(c)))
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
/// kind's cap, and so its guest thread.
#[must_use = "dropping the admission ends the call"]
pub struct Admitted {
    admission: Admission,
    kind: CallKind,
    queued: bool,
}

/// A top-level call that reached its deadline while queued.
#[derive(Debug, PartialEq, Eq)]
pub struct Expired;

impl Admission {
    pub fn new(capacity: Capacity) -> Self {
        Self(Arc::new(Mutex::new(State {
            capacity,
            running: [0; 2],
            admitted: [0; 5],
            next_waiter: 0,
            queued: Default::default(),
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
        let state = self.0.lock();
        class
            .kinds()
            .iter()
            .map(|k| state.queued[k.index()].len())
            .sum()
    }

    /// Top-level calls of `kind` queued now.
    pub fn queued_kind(&self, kind: CallKind) -> usize {
        self.0.lock().queued[kind.index()].len()
    }

    /// Calls of `kind` admitted now.
    pub fn admitted(&self, kind: CallKind) -> usize {
        self.0.lock().admitted[kind.index()]
    }

    /// Admit a top-level call of `kind` and give it a token (none for a
    /// lifecycle call), queueing until its kind has room and a token is free.
    /// Gives up at `deadline`. Cancel-safe: dropping the future leaves the
    /// queue.
    pub async fn admit(
        &self,
        kind: CallKind,
        deadline: Instant,
    ) -> Result<(Admitted, Option<Token>), Expired> {
        let (id, rx) = {
            let mut state = self.0.lock();
            let token_free = state.running_total() < state.capacity.tokens;
            if state.has_room(kind) && (kind.class().is_none() || token_free) {
                state.admitted[kind.index()] += 1;
                let token = kind.class().map(|class| {
                    state.running[class.index()] += 1;
                    self.token(class)
                });
                drop(state);
                return Ok((self.admission(kind, false), token));
            }
            let (tx, rx) = oneshot::channel();
            let id = state.next_waiter;
            state.next_waiter += 1;
            state.queued[kind.index()].insert(id, tx);
            (id, rx)
        };
        let mut waiting = Waiting {
            admission: self,
            kind,
            id,
            rx,
            granted: false,
        };
        match tokio::time::timeout_at(deadline.into(), &mut waiting.rx).await {
            Ok(Ok(token)) => {
                // The grant already counted this call as admitted.
                waiting.granted = true;
                Ok((self.admission(kind, true), token))
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

    fn admission(&self, kind: CallKind, queued: bool) -> Admitted {
        Admitted {
            admission: self.clone(),
            kind,
            queued,
        }
    }

    /// Hand out what freed up. Lifecycle calls need only room under their
    /// cap. A token goes to the class by share, then within it to a resuming
    /// call before the oldest queued call whose kind has room. Called with
    /// the lock held after a token or an admission frees up.
    fn grant(&self, state: &mut State) {
        let lifecycle = CallKind::Lifecycle.index();
        while state.has_room(CallKind::Lifecycle) {
            let Some((_, tx)) = state.queued[lifecycle].pop_first() else {
                break;
            };
            state.admitted[lifecycle] += 1;
            if tx.send(None).is_err() {
                state.admitted[lifecycle] -= 1;
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
            let Some(kind) = state.next_kind(class) else {
                return;
            };
            let k = kind.index();
            let Some((_, tx)) = state.queued[k].pop_first() else {
                return;
            };
            state.running[c] += 1;
            state.admitted[k] += 1;
            if let Err(Some(mut token)) = tx.send(Some(self.token(class))) {
                token.armed = false;
                state.running[c] -= 1;
                state.admitted[k] -= 1;
            }
        }
    }
}

/// A queued top-level call. Dropped before it was granted (its deadline
/// passed or its caller went away), it leaves the queue; if a grant raced the
/// drop, it hands the granted token and admission back.
struct Waiting<'a> {
    admission: &'a Admission,
    kind: CallKind,
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
            if state.queued[self.kind.index()].remove(&self.id).is_some() {
                None
            } else {
                // Granted under the lock before we took it: the grant is in
                // the channel and the call was counted as admitted.
                state.admitted[self.kind.index()] -= 1;
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
        self.kind
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
        state.admitted[self.kind.index()] -= 1;
        self.admission.grant(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const INTERACTIVE: CallKind = CallKind::Request;
    const BACKGROUND: CallKind = CallKind::Work;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    type Admit = (Admitted, Option<Token>);

    async fn admit(a: &Admission, kind: CallKind) -> Admit {
        a.admit(kind, far()).await.expect("admitted")
    }

    /// Queue an admission on its own task; returns its handle once queued.
    async fn queue(
        a: &Admission,
        kind: CallKind,
    ) -> tokio::task::JoinHandle<Result<Admit, Expired>> {
        let before = a.queued_kind(kind);
        let task = tokio::spawn({
            let a = a.clone();
            async move { a.admit(kind, far()).await }
        });
        while a.queued_kind(kind) == before {
            tokio::task::yield_now().await;
        }
        task
    }

    /// A capacity of `tokens` whose caps never bind.
    fn roomy(tokens: usize) -> Capacity {
        CallKind::ALL
            .into_iter()
            .fold(Capacity::new(tokens), |c, k| c.with_cap(k, 1000))
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn a_class_borrows_the_other_classes_idle_tokens() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, BACKGROUND).await);
        }
        assert_eq!(a.running(CallClass::Background), 4);
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn background_gets_the_next_token_under_an_interactive_flood() {
        let a = Admission::new(roomy(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, INTERACTIVE).await);
        }
        let interactive = queue(&a, INTERACTIVE).await;
        let background = queue(&a, BACKGROUND).await;
        held.pop();
        let _granted = background.await.unwrap().unwrap();
        assert_eq!(a.queued(CallClass::Interactive), 1);
        assert!(!interactive.is_finished());
    }

    #[tokio::test]
    async fn a_class_at_its_share_yields_to_the_other_but_not_to_itself() {
        let a = Admission::new(roomy(4));
        let mut interactive = Vec::new();
        for _ in 0..2 {
            interactive.push(admit(&a, INTERACTIVE).await);
        }
        let mut background = Vec::new();
        for _ in 0..2 {
            background.push(admit(&a, BACKGROUND).await);
        }
        let more_interactive = queue(&a, INTERACTIVE).await;
        let more_background = queue(&a, BACKGROUND).await;
        // Background frees one: background is now below its share, so the
        // token stays with background.
        background.pop();
        let _granted = more_background.await.unwrap().unwrap();
        assert!(!more_interactive.is_finished());
    }

    #[tokio::test]
    async fn a_dropped_waiter_leaves_the_queue() {
        let a = Admission::new(roomy(2));
        let _held = (admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await);
        let waiter = queue(&a, INTERACTIVE).await;
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test]
    async fn a_waiter_past_its_deadline_expires() {
        let a = Admission::new(roomy(2));
        let _held = (admit(&a, BACKGROUND).await, admit(&a, BACKGROUND).await);
        let expired = a
            .admit(INTERACTIVE, Instant::now())
            .await
            .err()
            .expect("no token was free before the deadline");
        assert_eq!(expired, Expired);
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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
            64 + DEFAULT_JUDGING_CAP + 16 + 8 + LIFECYCLE_CAP
        );
        assert_eq!(caps.threads(), 2 * caps.total());
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

    #[tokio::test]
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
