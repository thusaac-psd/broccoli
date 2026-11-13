//! Admission to run guest code.
//!
//! A server holds `C` CPU tokens. A plugin call takes one before it gets a
//! thread and holds it only while guest code runs: around every host function
//! that blocks it is parked (gives its token back) and resumed (takes one back
//! ahead of every queued call). So at most `C` guests run at once, and a call
//! parked for minutes in a host wait (a contest call waiting for its verdicts)
//! never keeps another call from running.
//!
//! Calls come in two classes. Each has a guaranteed share of the tokens and may
//! borrow the other's idle ones; a token freed while both classes wait goes to
//! the class below its share. Interactive calls also count against a cap on
//! admitted calls (running or parked), because each admitted call holds a
//! blocking thread: when the database slows every interactive call down, the
//! excess waits here, holding nothing, until its request deadline.
//!
//! A queued waiter is an async future. Dropping it leaves the queue, so a
//! request that went away no longer waits for a token.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::oneshot;

/// Which share of the CPU tokens a call draws from. Every top-level call names
/// one; nested calls inherit their parent's.
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

/// Tokens a server runs guests with, per available core. Guest code is CPU
/// bound, so one token per core would saturate the cores only if no call ever
/// stalled while holding a token; calls do stall briefly (building an
/// instance, a page fault on a fresh linear memory, the hop between the async
/// waiter and its blocking thread). Two per core keeps the cores busy through
/// those stalls while bounding the run queue to two guests per core.
pub const TOKENS_PER_CORE: usize = 2;

/// Admitted interactive calls (running or parked) allowed per CPU token. Each
/// holds a blocking thread; this keeps those threads to a small multiple of
/// the tokens however slow host functions get.
pub const INTERACTIVE_ADMITTED_PER_TOKEN: usize = 8;

/// The shape of a server's admission: how many tokens, and how they split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub tokens: usize,
    /// Guaranteed tokens per class, indexed like [`CallClass`]; they sum to
    /// `tokens`.
    shares: [usize; 2],
    interactive_admitted: usize,
}

impl Capacity {
    /// `tokens` CPU tokens split evenly between the classes (interactive gets
    /// the odd one).
    pub fn new(tokens: usize) -> Self {
        let tokens = tokens.max(2);
        let interactive = tokens.div_ceil(2);
        Self {
            tokens,
            shares: [interactive, tokens - interactive],
            interactive_admitted: tokens * INTERACTIVE_ADMITTED_PER_TOKEN,
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
}

struct State {
    capacity: Capacity,
    running: [usize; 2],
    admitted: [usize; 2],
    next_waiter: u64,
    /// Queued top-level calls per class, oldest first.
    queued: [BTreeMap<u64, oneshot::Sender<Token>>; 2],
    /// Parked calls returning from a host function, served before any queue.
    resuming: std::collections::VecDeque<(CallClass, SyncSender<Token>)>,
}

impl State {
    fn running_total(&self) -> usize {
        self.running.iter().sum()
    }

    fn may_admit(&self, class: CallClass) -> bool {
        class != CallClass::Interactive
            || self.admitted[class.index()] < self.capacity.interactive_admitted
    }

    /// The class the next free token goes to, if any queued call may take it.
    fn next_class(&self) -> Option<CallClass> {
        let waiting = |c: CallClass| !self.queued[c.index()].is_empty() && self.may_admit(c);
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

/// An admitted top-level call, held for the whole call. Counts against the
/// interactive admitted cap.
#[must_use = "dropping the admission ends the call"]
pub struct Admitted {
    admission: Admission,
    class: CallClass,
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
            admitted: [0; 2],
            next_waiter: 0,
            queued: [BTreeMap::new(), BTreeMap::new()],
            resuming: Default::default(),
        })))
    }

    pub fn capacity(&self) -> Capacity {
        self.0.lock().capacity
    }

    /// Guests running now, per class.
    pub fn running(&self, class: CallClass) -> usize {
        self.0.lock().running[class.index()]
    }

    /// Top-level calls queued now, per class.
    pub fn queued(&self, class: CallClass) -> usize {
        self.0.lock().queued[class.index()].len()
    }

    /// Admit a top-level call of `class` and give it a token, queueing until
    /// one is free. Gives up at `deadline`. Cancel-safe: dropping the future
    /// leaves the queue.
    pub async fn admit(
        &self,
        class: CallClass,
        deadline: Instant,
    ) -> Result<(Admitted, Token), Expired> {
        let (id, rx) = {
            let mut state = self.0.lock();
            if state.queued[class.index()].is_empty()
                && state.running_total() < state.capacity.tokens
                && state.may_admit(class)
            {
                state.running[class.index()] += 1;
                state.admitted[class.index()] += 1;
                drop(state);
                return Ok((self.admitted(class, false), self.token(class)));
            }
            let (tx, rx) = oneshot::channel();
            let id = state.next_waiter;
            state.next_waiter += 1;
            state.queued[class.index()].insert(id, tx);
            (id, rx)
        };
        let mut waiting = Waiting {
            admission: self,
            class,
            id,
            rx,
            granted: false,
        };
        match tokio::time::timeout_at(deadline.into(), &mut waiting.rx).await {
            Ok(Ok(token)) => {
                // The grant already counted this call as admitted.
                waiting.granted = true;
                Ok((self.admitted(class, true), token))
            }
            // The sender is only dropped after a failed delivery, which
            // cannot happen while `waiting.rx` is alive.
            Ok(Err(_)) | Err(_) => Err(Expired),
        }
    }

    /// Take a token for a parked call of `class` returning to guest code,
    /// ahead of every queued call. Blocks the calling thread; gives up at
    /// `deadline`, after which the guest is cancelled anyway.
    pub fn resume(&self, class: CallClass, deadline: Instant) -> Option<Token> {
        let rx: Receiver<Token> = {
            let mut state = self.0.lock();
            if state.running_total() < state.capacity.tokens {
                state.running[class.index()] += 1;
                drop(state);
                return Some(self.token(class));
            }
            let (tx, rx) = sync_channel(1);
            state.resuming.push_back((class, tx));
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

    fn admitted(&self, class: CallClass, queued: bool) -> Admitted {
        Admitted {
            admission: self.clone(),
            class,
            queued,
        }
    }

    /// Hand freed tokens to waiters, resuming calls first. Called with the
    /// lock held after a token or an admission frees up.
    fn grant(&self, state: &mut State) {
        while state.running_total() < state.capacity.tokens {
            if let Some((class, tx)) = state.resuming.pop_front() {
                state.running[class.index()] += 1;
                if let Err(TrySendError::Full(mut token) | TrySendError::Disconnected(mut token)) =
                    tx.try_send(self.token(class))
                {
                    // The resumer stopped waiting; the token stays free.
                    token.armed = false;
                    state.running[class.index()] -= 1;
                }
                continue;
            }
            let Some(class) = state.next_class() else {
                return;
            };
            let Some((_, tx)) = state.queued[class.index()].pop_first() else {
                return;
            };
            state.running[class.index()] += 1;
            state.admitted[class.index()] += 1;
            if let Err(mut token) = tx.send(self.token(class)) {
                token.armed = false;
                state.running[class.index()] -= 1;
                state.admitted[class.index()] -= 1;
            }
        }
    }
}

/// A queued top-level call. Dropped before it was granted (its deadline
/// passed or its caller went away), it leaves the queue; if a grant raced the
/// drop, it hands the granted token and admission back.
struct Waiting<'a> {
    admission: &'a Admission,
    class: CallClass,
    id: u64,
    rx: oneshot::Receiver<Token>,
    granted: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.granted {
            return;
        }
        let raced = {
            let mut state = self.admission.0.lock();
            if state.queued[self.class.index()].remove(&self.id).is_some() {
                None
            } else {
                // Granted under the lock before we took it: the token is in
                // the channel and the call was counted as admitted.
                state.admitted[self.class.index()] -= 1;
                self.rx.try_recv().ok()
            }
        };
        // Released outside the lock: the token's drop takes it again.
        drop(raced);
    }
}

impl Admitted {
    /// Whether the call had to queue for its token.
    pub fn queued(&self) -> bool {
        self.queued
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
        state.admitted[self.class.index()] -= 1;
        self.admission.grant(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    async fn admit(a: &Admission, class: CallClass) -> (Admitted, Token) {
        a.admit(class, far()).await.expect("admitted")
    }

    /// Queue an admission on its own task; returns its handle once queued.
    async fn queue(
        a: &Admission,
        class: CallClass,
    ) -> tokio::task::JoinHandle<Result<(Admitted, Token), Expired>> {
        let before = a.queued(class);
        let task = tokio::spawn({
            let a = a.clone();
            async move { a.admit(class, far()).await }
        });
        while a.queued(class) == before {
            tokio::task::yield_now().await;
        }
        task
    }

    #[tokio::test]
    async fn never_more_tokens_out_than_capacity() {
        let a = Admission::new(Capacity::new(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, CallClass::Background).await);
        }
        let waiter = queue(&a, CallClass::Background).await;
        assert_eq!(a.running(CallClass::Background), 4);
        held.pop();
        let _granted = waiter.await.unwrap().unwrap();
        assert_eq!(a.running(CallClass::Background), 4);
    }

    #[tokio::test]
    async fn a_class_borrows_the_other_classes_idle_tokens() {
        let a = Admission::new(Capacity::new(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, CallClass::Background).await);
        }
        assert_eq!(a.running(CallClass::Background), 4);
    }

    #[tokio::test]
    async fn interactive_gets_the_next_token_under_a_background_flood() {
        let a = Admission::new(Capacity::new(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, CallClass::Background).await);
        }
        let background = queue(&a, CallClass::Background).await;
        let interactive = queue(&a, CallClass::Interactive).await;
        held.pop();
        let _granted = interactive.await.unwrap().unwrap();
        assert_eq!(a.queued(CallClass::Background), 1);
        assert!(!background.is_finished());
    }

    #[tokio::test]
    async fn background_gets_the_next_token_under_an_interactive_flood() {
        let a = Admission::new(Capacity::new(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admit(&a, CallClass::Interactive).await);
        }
        let interactive = queue(&a, CallClass::Interactive).await;
        let background = queue(&a, CallClass::Background).await;
        held.pop();
        let _granted = background.await.unwrap().unwrap();
        assert_eq!(a.queued(CallClass::Interactive), 1);
        assert!(!interactive.is_finished());
    }

    #[tokio::test]
    async fn a_class_at_its_share_yields_to_the_other_but_not_to_itself() {
        let a = Admission::new(Capacity::new(4));
        let mut interactive = Vec::new();
        for _ in 0..2 {
            interactive.push(admit(&a, CallClass::Interactive).await);
        }
        let mut background = Vec::new();
        for _ in 0..2 {
            background.push(admit(&a, CallClass::Background).await);
        }
        let more_interactive = queue(&a, CallClass::Interactive).await;
        let more_background = queue(&a, CallClass::Background).await;
        // Background frees one: background is now below its share, so the
        // token stays with background.
        background.pop();
        let _granted = more_background.await.unwrap().unwrap();
        assert!(!more_interactive.is_finished());
    }

    #[tokio::test]
    async fn a_dropped_waiter_leaves_the_queue() {
        let a = Admission::new(Capacity::new(2));
        let _held = (
            admit(&a, CallClass::Background).await,
            admit(&a, CallClass::Background).await,
        );
        let waiter = queue(&a, CallClass::Interactive).await;
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test]
    async fn a_waiter_past_its_deadline_expires() {
        let a = Admission::new(Capacity::new(2));
        let _held = (
            admit(&a, CallClass::Background).await,
            admit(&a, CallClass::Background).await,
        );
        let expired = a
            .admit(CallClass::Interactive, Instant::now())
            .await
            .err()
            .expect("no token was free before the deadline");
        assert_eq!(expired, Expired);
        assert_eq!(a.queued(CallClass::Interactive), 0);
    }

    #[tokio::test]
    async fn a_resuming_call_goes_ahead_of_every_queued_call() {
        let a = Admission::new(Capacity::new(2));
        let (admitted, parked_token) = admit(&a, CallClass::Background).await;
        let other = admit(&a, CallClass::Background).await;
        let queued = queue(&a, CallClass::Interactive).await;
        // The first call parks: its token goes to the queued call.
        drop(parked_token);
        let granted = queued.await.unwrap().unwrap();
        // Now it wants back in while another call queues.
        let late = queue(&a, CallClass::Interactive).await;
        let resumer = std::thread::spawn({
            let a = a.clone();
            move || a.resume(CallClass::Background, far()).map(|t| t.class())
        });
        while a.0.lock().resuming.is_empty() {
            std::thread::yield_now();
        }
        drop(other);
        assert_eq!(resumer.join().unwrap(), Some(CallClass::Background));
        assert!(!late.is_finished());
        drop((admitted, granted));
    }

    #[tokio::test]
    async fn the_interactive_admitted_cap_holds_parked_calls_too() {
        let a = Admission::new(Capacity::new(2));
        let cap = a.capacity().interactive_admitted;
        let mut parked = Vec::new();
        for _ in 0..cap {
            let (admitted, token) = admit(&a, CallClass::Interactive).await;
            drop(token);
            parked.push(admitted);
        }
        let waiter = queue(&a, CallClass::Interactive).await;
        assert_eq!(a.running(CallClass::Interactive), 0);
        // Background still runs; only interactive is capped.
        let _background = admit(&a, CallClass::Background).await;
        parked.pop();
        let _granted = waiter.await.unwrap().unwrap();
    }
}
