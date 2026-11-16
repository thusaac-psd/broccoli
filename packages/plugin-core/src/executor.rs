//! How a plugin call runs.
//!
//! Every call names how it is admitted ([`CallOptions`]) and ends by a
//! deadline:
//!
//! - A top-level call of a [`CallKind`] waits, holding nothing but a queue
//!   entry, until its kind has room and a CPU token is free
//!   ([`crate::admission`]). Only then does it get a guest thread
//!   ([`crate::threads`]) and look up the plugin's pool, so a call queued
//!   across a reload runs on the new version.
//! - A lifecycle call (plugin `init`) needs room under its own small cap but
//!   no token: it is never shed and never queues behind either class.
//! - A nested call (a host function calling back into a plugin) skips
//!   admission, inheriting its parent's class and the earlier of both
//!   deadlines. Its parent is parked in that host function, holding no token;
//!   the nested call runs on a second guest thread (host functions
//!   `block_on`, which cannot nest on one thread) and takes a token back the
//!   way a resuming call does. Nesting is capped at [`MAX_DEPTH`], which stops
//!   A-calls-B-calls-A cycles and gives every admitted call at most one
//!   nested call, the second thread the thread budget reserves for it.
//!
//! While guest code runs, the thread carries a call scope. A host function
//! that blocks wraps the blocking part in [`parked`], which gives the token
//! back for the duration and takes one back, ahead of every queued call, when
//! it returns. A call still queued at its deadline, or cancelled at it by the
//! deadline thread, fails with
//! [`PluginError::Unavailable`].

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::admission::{Admission, CallClass, CallKind, Capacity, Token};
use crate::error::PluginError;
use crate::memory::MemoryBudget;

/// Deepest nesting: a top-level call and one nested call.
pub const MAX_DEPTH: u8 = 2;

/// How a call is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admit {
    Kind(CallKind, Instant),
    Nested,
}

/// How one call is admitted and when it must end. There is no default: every
/// call site states its kind and deadline, or that it is nested.
#[derive(Debug, Clone, Copy)]
pub struct CallOptions {
    admit: Admit,
}

impl CallOptions {
    /// A top-level call of `kind` that must end by `deadline`.
    pub fn new(kind: CallKind, deadline: Instant) -> Self {
        Self {
            admit: Admit::Kind(kind, deadline),
        }
    }

    /// A request path call: visibility, request hooks, plugin routes.
    pub fn interactive(deadline: Instant) -> Self {
        Self::new(CallKind::Request, deadline)
    }

    /// A contest call that waits in host functions for its judgement.
    pub fn judging(deadline: Instant) -> Self {
        Self::new(CallKind::Judging, deadline)
    }

    /// A background call that waits on no other plugin call: evaluators,
    /// detached callbacks, timers.
    pub fn background(deadline: Instant) -> Self {
        Self::new(CallKind::Work, deadline)
    }

    /// A hook dispatched off the request path.
    pub fn background_hook(deadline: Instant) -> Self {
        Self::new(CallKind::Hook, deadline)
    }

    /// A plugin lifecycle call (its `init`): needs no token, whatever the
    /// load.
    pub fn lifecycle(deadline: Instant) -> Self {
        Self::new(CallKind::Lifecycle, deadline)
    }

    /// A call from inside a host function of a running call: inherits that
    /// call's class and deadline (bounded by the call timeout).
    pub fn nested() -> Self {
        Self {
            admit: Admit::Nested,
        }
    }
}

/// The admission and memory state every call of one server shares.
pub struct Executor {
    admission: Admission,
    threads: crate::threads::GuestThreads,
    budget: std::sync::Arc<MemoryBudget>,
}

impl Executor {
    pub fn new(capacity: Capacity, budget: std::sync::Arc<MemoryBudget>) -> Self {
        Self {
            admission: Admission::new(capacity),
            threads: Default::default(),
            budget,
        }
    }

    /// The threads guests run on.
    pub fn threads(&self) -> &crate::threads::GuestThreads {
        &self.threads
    }

    /// Sized from this host: `max_concurrent_calls` tokens (or the automatic
    /// count) and `memory_budget` bytes (or the automatic budget).
    pub fn for_this_host(max_concurrent_calls: Option<usize>, memory_budget: Option<u64>) -> Self {
        if let Some(tokens) = max_concurrent_calls
            && tokens < crate::admission::MIN_TOKENS
        {
            tracing::warn!(
                configured = tokens,
                used = crate::admission::MIN_TOKENS,
                "plugin.max_concurrent_calls is below one token per call class; raising it"
            );
        }
        Self::new(
            Capacity::for_config(max_concurrent_calls),
            MemoryBudget::for_this_host(memory_budget),
        )
    }

    /// One shared executor sized from this host, for test doubles that
    /// never run guests themselves.
    pub fn shared_for_tests() -> &'static Executor {
        static SHARED: std::sync::LazyLock<Executor> =
            std::sync::LazyLock::new(|| Executor::for_this_host(None, None));
        &SHARED
    }

    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    pub fn budget(&self) -> &std::sync::Arc<MemoryBudget> {
        &self.budget
    }
}

/// What the thread running a guest knows about its call.
struct CallScope {
    admission: Option<Admission>,
    class: Option<CallClass>,
    deadline: Instant,
    depth: u8,
    /// Held while guest code runs; `None` while parked, and always for a
    /// lifecycle call.
    token: Option<Token>,
}

thread_local! {
    static SCOPE: RefCell<Option<CallScope>> = const { RefCell::new(None) };
}

/// The parent a nested call inherits from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Parent {
    pub class: Option<CallClass>,
    pub deadline: Instant,
    pub depth: u8,
}

/// The call running on this thread, if any.
pub(crate) fn current_parent() -> Option<Parent> {
    SCOPE.with(|scope| {
        scope.borrow().as_ref().map(|s| Parent {
            class: s.class,
            deadline: s.deadline,
            depth: s.depth,
        })
    })
}

/// The deadline of the call running on this thread, if any. The host's result
/// waits (`get_next_evaluate_result`, `get_next_operation_result`) end at it;
/// its database waits are bounded by the statement timeout.
pub fn current_deadline() -> Option<Instant> {
    SCOPE.with(|scope| scope.borrow().as_ref().map(|s| s.deadline))
}

/// Installs a call's scope on this thread; restores the previous one on drop,
/// including while unwinding.
pub(crate) struct ScopeGuard {
    previous: Option<CallScope>,
}

pub(crate) struct NewScope {
    pub admission: Option<Admission>,
    pub class: Option<CallClass>,
    pub deadline: Instant,
    pub depth: u8,
    pub token: Option<Token>,
}

impl ScopeGuard {
    pub(crate) fn enter(new: NewScope) -> Self {
        let scope = CallScope {
            admission: new.admission,
            class: new.class,
            deadline: new.deadline,
            depth: new.depth,
            token: new.token,
        };
        let previous = SCOPE.with(|s| s.borrow_mut().replace(scope));
        Self { previous }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let ended = SCOPE.with(|s| std::mem::replace(&mut *s.borrow_mut(), self.previous.take()));
        // The token is released after the thread-local is restored: its drop
        // takes the admission lock.
        drop(ended);
    }
}

/// Run `wait`, a host function's blocking part, with this thread's call
/// parked: its token is free for other calls until `wait` returns, then it
/// takes one back ahead of every queued call. Outside a call (or in a
/// lifecycle call, which holds no token) it just runs `wait`.
pub fn parked<R>(wait: impl FnOnce() -> R) -> R {
    let _parked = Parked::enter();
    wait()
}

/// A parked call; see [`parked`]. Takes the token back on drop, except while
/// unwinding, when the call is ending anyway.
#[must_use = "the call resumes when this is dropped"]
pub struct Parked {
    resume: Option<(Admission, CallClass, Instant)>,
}

impl Parked {
    pub fn enter() -> Self {
        let released = SCOPE.with(|scope| {
            let mut scope = scope.borrow_mut();
            let scope = scope.as_mut()?;
            let token = scope.token.take()?;
            Some((token, scope.admission.clone()?, scope.deadline))
        });
        let resume = released.map(|(token, admission, deadline)| {
            let class = token.class();
            drop(token);
            (admission, class, deadline)
        });
        Self { resume }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        let Some((admission, class, deadline)) = self.resume.take() else {
            return;
        };
        if std::thread::panicking() {
            return;
        }
        // At the deadline the token is not worth waiting for: the guest is
        // cancelled as soon as it runs.
        let token = admission.resume(class, deadline);
        SCOPE.with(|scope| {
            if let Some(scope) = scope.borrow_mut().as_mut() {
                scope.token = token;
            }
        });
    }
}

/// Resolve `options` for a call starting now: the admission class, the
/// deadline and the nesting depth. `call_timeout` bounds a nested call.
pub(crate) fn resolve(
    options: CallOptions,
    call_timeout: Duration,
    plugin_id: &str,
    func_name: &str,
) -> Result<Resolved, PluginError> {
    match options.admit {
        Admit::Kind(kind, deadline) => Ok(Resolved {
            queue: Some(kind),
            class: kind.class(),
            deadline,
            depth: 1,
        }),
        Admit::Nested => {
            let parent = current_parent().ok_or_else(|| {
                PluginError::Internal(format!(
                    "nested call to '{func_name}' on plugin '{plugin_id}' outside a plugin call"
                ))
            })?;
            if parent.depth >= MAX_DEPTH {
                return Err(PluginError::NestingTooDeep {
                    plugin_id: plugin_id.to_string(),
                    func_name: func_name.to_string(),
                    max_depth: MAX_DEPTH,
                });
            }
            Ok(Resolved {
                queue: None,
                class: parent.class,
                deadline: parent.deadline.min(Instant::now() + call_timeout),
                depth: parent.depth + 1,
            })
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Resolved {
    /// The kind to admit as; `None` (a nested call) skips admission.
    pub queue: Option<CallKind>,
    /// The class guest code runs as, for parking and resuming; `None` for a
    /// lifecycle call, which never holds a token.
    pub class: Option<CallClass>,
    pub deadline: Instant,
    pub depth: u8,
}
