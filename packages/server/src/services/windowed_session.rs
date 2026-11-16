//! Shared driver for a detached, windowed, callback-per-result session.
//!
//! Both `evaluate_batch` (test cases -> verdicts) and `operation_batch`
//! (operation tasks -> results) run the same host-side loop: keep up to
//! `concurrency` single-item batches in flight, await each result, hand it to
//! the plugin's callback, and act on the callback's reply (continue / finish /
//! cancel, refill the window, cancel specific slots). That loop is factored out
//! here as [`run_windowed_session`]; a domain plugs in the pieces that genuinely
//! differ via [`WindowedSession`]:
//!
//! - **dispatch/transport** (`start_slot`): evaluate calls the evaluator plugin
//!   in-process; operation round-trips a task through the MQ + a worker.
//! - **result decode** (`decode`): operation must deserialize a raw worker
//!   `TaskResult` into an `OperationResult` and can fail into a Timeout;
//!   evaluate's verdict already arrives typed.
//! - **cancellation** (`cancel_active`/`cancel_selected`): both async; evaluate
//!   additionally signals Redis op-cancel keys.
//! - **server-side retry** (`take_retry`/`note_retry*`): evaluate re-dispatches a
//!   timed-out op a bounded number of times before surfacing the timeout;
//!   operation never retries (the default no-ops).
//! - **the end** (`on_end`): called once on every exit with a [`SessionEnd`];
//!   evaluate finalizes the judgement the session judges and fires its
//!   after-judging hooks; operation has nothing to record.
//!
//! The callback protocol itself is isomorphic across the two domains (a
//! Continue/Finish/Cancel action, a `refill` bool, and a per-slot cancel list),
//! so the loop drives it through the normalized [`SessionDecision`].

use std::collections::HashSet;
use std::hash::Hash;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

/// Normalized terminal action from a detached callback. Both domains' concrete
/// `Action` enums map onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Continue,
    Finish,
    Cancel,
}

/// A detached callback's reply, normalized across domains.
pub struct SessionDecision<SlotId> {
    pub state: serde_json::Value,
    pub action: SessionAction,
    pub refill: bool,
    /// Slot ids the callback asked to cancel (evaluate: test case ids;
    /// operation: operation indices).
    pub cancel_ids: Vec<SlotId>,
}

/// The event the loop feeds the plugin callback for one delivered slot. The loop
/// has already collapsed `Ok(None)` / `Err` / decode failures into `Timeout`.
pub enum SlotEvent<SlotId, Final> {
    Result { slot_id: SlotId, result: Final },
    Timeout { message: String },
    Exhausted,
}

/// One slot's delivered outcome, produced by the per-slot waiter task and sent
/// back to the loop. `result` is the raw (pre-decode) result: `Ok(Some(..))`
/// delivered, `Ok(None)` timed out, `Err(..)` the wait errored.
pub struct SlotOutcome<SlotId, Raw> {
    pub slot_id: SlotId,
    pub batch_id: String,
    pub result: anyhow::Result<Option<Raw>>,
}

/// The domain-specific pieces of a detached windowed session. See the module
/// docs for what each hook is for.
// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait WindowedSession: Send + Sync + Sized + 'static {
    /// A pending work item (evaluate: a test case; operation: an operation task).
    type Item: Send + 'static;
    /// The raw result delivered by a slot's waiter (evaluate: a verdict;
    /// operation: a raw worker `TaskResult`).
    type Raw: Send + 'static;
    /// The decoded result handed to the callback (evaluate: verdict; operation:
    /// `OperationResult`).
    type Final: Send;
    /// A slot discriminator (evaluate: `i32` test case id; operation: `usize`
    /// index). Used to match callback cancel lists and retry bookkeeping.
    type SlotId: Copy + Eq + Hash + Send + 'static;

    /// Plugin id and session id, for log context only.
    fn plugin_id(&self) -> &str;
    fn session_id(&self) -> &str;

    /// The slot id for the `index`-th pending item. operation: `index`;
    /// evaluate: the item's own test case id.
    fn slot_id(&self, index: usize, item: &Self::Item) -> Self::SlotId;

    /// Dispatch `item` as a single-item batch and spawn its waiter task, which
    /// must send exactly one [`SlotOutcome`] (carrying `slot_id` and the returned
    /// batch id) into `tx`. Returns the batch id.
    async fn start_slot(
        &self,
        slot_id: Self::SlotId,
        item: Self::Item,
        timeout: Duration,
        tx: mpsc::Sender<SlotOutcome<Self::SlotId, Self::Raw>>,
    ) -> anyhow::Result<String>;

    /// Decode a delivered raw result into the final result, or an error message
    /// that becomes a Timeout event. evaluate: identity; operation: raw
    /// `TaskResult` -> `OperationResult`.
    fn decode(&self, raw: Self::Raw) -> Result<Self::Final, String>;

    /// The Timeout message for a slot that delivered `Ok(None)`.
    fn timeout_message(&self, slot_id: Self::SlotId) -> String;

    /// Build the domain callback input from `event`, invoke the plugin, and
    /// normalize its reply.
    async fn call_callback(
        &self,
        event: SlotEvent<Self::SlotId, Self::Final>,
        state: serde_json::Value,
        completed: usize,
        total: usize,
    ) -> anyhow::Result<SessionDecision<Self::SlotId>>;

    /// Cancel every still-active slot's batch.
    async fn cancel_active(&self, active: &[(Self::SlotId, String)]);

    /// Cancel the active slots whose id is in `ids`, retaining the rest.
    async fn cancel_selected(
        &self,
        active: &mut Vec<(Self::SlotId, String)>,
        ids: &HashSet<Self::SlotId>,
    );

    /// On a timed-out slot, optionally provide an item to re-dispatch on a fresh
    /// batch (server-side retry, before the timeout is surfaced to the plugin).
    /// Default: never retry.
    async fn take_retry(&mut self, _slot_id: Self::SlotId, _batch_id: &str) -> Option<Self::Item> {
        None
    }

    /// Record a successful retry re-dispatch (`old_batch_id` was superseded).
    /// Default no-op.
    fn note_retry(&mut self, _slot_id: Self::SlotId, _old_batch_id: &str) {}

    /// Record a failed retry re-dispatch; the timeout will be surfaced instead.
    /// Default no-op.
    fn note_retry_failed(&self, _slot_id: Self::SlotId, _error: &anyhow::Error) {}

    /// Called exactly once, when the session has ended and its active slots
    /// are cancelled. A driver whose session judges a unit must leave that
    /// unit terminal here, whatever `end` is.
    async fn on_end(&self, end: SessionEnd);
}

/// Remove the active slot with `batch_id`; returns whether one was found. A late
/// result for an already-removed (cancelled/superseded) slot is ignored.
fn remove_active_slot<S: Copy>(active: &mut Vec<(S, String)>, batch_id: &str) -> bool {
    let Some(index) = active.iter().position(|(_, id)| id == batch_id) else {
        return false;
    };
    active.swap_remove(index);
    true
}

/// How a session ended. Every run of [`run_windowed_session`] ends in exactly
/// one of these, handed once to [`WindowedSession::on_end`] after every active
/// slot is cancelled, so no exit can leave the session's work unaccounted for.
#[derive(Debug)]
pub enum SessionEnd {
    /// The plugin's callback ended it with this action: `Finish` or `Cancel`,
    /// or whatever it answered to the `Exhausted` event.
    Decided(SessionAction),
    /// The plugin could not decide: its callback failed or the plugin is gone.
    Aborted(String),
    /// This server is shutting down. The session's work is released with the
    /// server's leases and judged again elsewhere, so nothing is recorded.
    Interrupted,
}

impl SessionEnd {
    /// The end that a failed callback call means.
    fn of_callback_error(error: &anyhow::Error) -> Self {
        let shutting_down = error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<plugin_core::error::PluginError>(),
                Some(plugin_core::error::PluginError::ShuttingDown)
            )
        });
        if shutting_down {
            Self::Interrupted
        } else {
            Self::Aborted(format!("{error:#}"))
        }
    }
}

/// Drive a detached windowed session to completion. Spawned as a background task
/// by each domain's `start_detached_windowed_*`.
pub async fn run_windowed_session<S: WindowedSession>(
    mut driver: S,
    items: Vec<S::Item>,
    concurrency: usize,
    timeout: Duration,
    state: serde_json::Value,
) {
    let mut active = Vec::<(S::SlotId, String)>::new();
    let end = drive(&mut driver, &mut active, items, concurrency, timeout, state).await;
    if let SessionEnd::Aborted(error) = &end {
        tracing::error!(
            plugin_id = %driver.plugin_id(),
            session_id = %driver.session_id(),
            error = %error,
            "Detached windowed session aborted"
        );
    }
    driver.cancel_active(&active).await;
    driver.on_end(end).await;
}

/// Ask the plugin's callback about `event`; a failed call ends the session.
async fn decide<S: WindowedSession>(
    driver: &S,
    event: SlotEvent<S::SlotId, S::Final>,
    state: serde_json::Value,
    completed: usize,
    total: usize,
) -> Result<SessionDecision<S::SlotId>, SessionEnd> {
    driver
        .call_callback(event, state, completed, total)
        .await
        .map_err(|e| SessionEnd::of_callback_error(&e))
}

/// The session loop. Returns how it ended, leaving `active` holding the slots
/// still in flight.
async fn drive<S: WindowedSession>(
    driver: &mut S,
    active: &mut Vec<(S::SlotId, String)>,
    items: Vec<S::Item>,
    concurrency: usize,
    timeout: Duration,
    mut state: serde_json::Value,
) -> SessionEnd {
    let total = items.len();
    let concurrency = concurrency.max(1);
    // `(slot_id, item)`, reversed so `pop()` dispatches in original order.
    let mut pending: Vec<(S::SlotId, S::Item)> = items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (driver.slot_id(index, &item), item))
        .collect();
    pending.reverse();
    let (tx, mut rx) = mpsc::channel::<SlotOutcome<S::SlotId, S::Raw>>(concurrency * 2);
    let mut completed = 0usize;

    // Fill the initial window.
    while active.len() < concurrency {
        let Some((slot_id, item)) = pending.pop() else {
            break;
        };
        match driver.start_slot(slot_id, item, timeout, tx.clone()).await {
            Ok(batch_id) => active.push((slot_id, batch_id)),
            Err(e) => {
                // The initial batch could not start: surface the failure to the
                // plugin and end the session.
                let event = SlotEvent::Timeout {
                    message: e.to_string(),
                };
                return match decide(driver, event, state, completed, total).await {
                    Ok(decision) => SessionEnd::Decided(decision.action),
                    Err(end) => end,
                };
            }
        }
    }

    if active.is_empty() {
        // Nothing was dispatched at all (an empty batch): give the plugin its
        // Exhausted event.
        return match decide(driver, SlotEvent::Exhausted, state, completed, total).await {
            Ok(decision) => SessionEnd::Decided(decision.action),
            Err(end) => end,
        };
    }

    while let Some(item) = rx.recv().await {
        if !remove_active_slot(active, &item.batch_id) {
            // A late result for a slot already removed (cancelled/superseded).
            continue;
        }

        // Server-side retry: a timed-out slot may be re-dispatched on a fresh
        // batch before the timeout is ever surfaced to the plugin. A retried slot
        // does not count toward `completed` and does not invoke the callback.
        let is_timeout = !matches!(item.result, Ok(Some(_)));
        if is_timeout
            && let Some(retry_item) = driver.take_retry(item.slot_id, &item.batch_id).await
        {
            match driver
                .start_slot(item.slot_id, retry_item, timeout, tx.clone())
                .await
            {
                Ok(new_batch_id) => {
                    driver.note_retry(item.slot_id, &item.batch_id);
                    active.push((item.slot_id, new_batch_id));
                    continue;
                }
                Err(e) => {
                    // Re-dispatch failed: fall through and surface the
                    // timeout rather than silently dropping the slot.
                    driver.note_retry_failed(item.slot_id, &e);
                }
            }
        }

        completed += 1;

        let event = match item.result {
            Ok(Some(raw)) => match driver.decode(raw) {
                Ok(result) => SlotEvent::Result {
                    slot_id: item.slot_id,
                    result,
                },
                Err(message) => SlotEvent::Timeout { message },
            },
            Ok(None) => SlotEvent::Timeout {
                message: driver.timeout_message(item.slot_id),
            },
            Err(e) => SlotEvent::Timeout {
                message: e.to_string(),
            },
        };

        let decision = match decide(driver, event, state.clone(), completed, total).await {
            Ok(decision) => decision,
            Err(end) => return end,
        };

        state = decision.state;
        let refill_enabled = decision.refill;

        if !decision.cancel_ids.is_empty() {
            let cancel_set: HashSet<S::SlotId> = decision.cancel_ids.iter().copied().collect();
            driver.cancel_selected(active, &cancel_set).await;
            pending.retain(|(slot_id, _)| !cancel_set.contains(slot_id));
        }

        match decision.action {
            SessionAction::Continue => {}
            SessionAction::Finish | SessionAction::Cancel => {
                return SessionEnd::Decided(decision.action);
            }
        }

        // Refill the window with pending items.
        while refill_enabled && active.len() < concurrency {
            let Some((slot_id, item)) = pending.pop() else {
                break;
            };
            match driver.start_slot(slot_id, item, timeout, tx.clone()).await {
                Ok(batch_id) => active.push((slot_id, batch_id)),
                Err(e) => {
                    tracing::error!(
                        plugin_id = %driver.plugin_id(),
                        session_id = %driver.session_id(),
                        error = %e,
                        "Failed to refill detached windowed slot"
                    );
                    let event = SlotEvent::Timeout {
                        message: e.to_string(),
                    };
                    let decision =
                        match decide(driver, event, state.clone(), completed, total).await {
                            Ok(decision) => decision,
                            Err(end) => return end,
                        };
                    state = decision.state;
                    match decision.action {
                        SessionAction::Continue => {}
                        SessionAction::Finish | SessionAction::Cancel => {
                            return SessionEnd::Decided(decision.action);
                        }
                    }
                    // `continue`, NOT `break`: the failed item was already
                    // surfaced to the plugin (which chose Continue), so drop it
                    // and try the remaining pending. Breaking here could leave
                    // `active` empty while `pending` is not, and the outer
                    // `rx.recv()` would then wait forever with no slot in
                    // flight to wake it.
                    continue;
                }
            }
        }

        if active.is_empty() && (!refill_enabled || pending.is_empty()) {
            break;
        }
    }

    match decide(driver, SlotEvent::Exhausted, state, completed, total).await {
        Ok(decision) => SessionEnd::Decided(decision.action),
        Err(end) => end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A driver whose slots deliver at once and whose callback answers from a
    /// script, recording how the session ended.
    struct Scripted {
        answers: Mutex<Vec<anyhow::Result<SessionAction>>>,
        ends: std::sync::Arc<Mutex<Vec<String>>>,
    }

    impl Scripted {
        /// Answers are taken in order, one per callback.
        fn new(answers: Vec<anyhow::Result<SessionAction>>) -> Self {
            let mut answers = answers;
            answers.reverse();
            Self {
                answers: Mutex::new(answers),
                ends: Default::default(),
            }
        }
    }

    #[async_trait]
    impl WindowedSession for Scripted {
        type Item = ();
        type Raw = ();
        type Final = ();
        type SlotId = usize;

        fn plugin_id(&self) -> &str {
            "p"
        }

        fn session_id(&self) -> &str {
            "s"
        }

        fn slot_id(&self, index: usize, _item: &()) -> usize {
            index
        }

        async fn start_slot(
            &self,
            slot_id: usize,
            _item: (),
            _timeout: Duration,
            tx: mpsc::Sender<SlotOutcome<usize, ()>>,
        ) -> anyhow::Result<String> {
            let batch_id = format!("b{slot_id}");
            tx.send(SlotOutcome {
                slot_id,
                batch_id: batch_id.clone(),
                result: Ok(Some(())),
            })
            .await
            .unwrap();
            Ok(batch_id)
        }

        fn decode(&self, raw: ()) -> Result<(), String> {
            Ok(raw)
        }

        fn timeout_message(&self, _slot_id: usize) -> String {
            String::new()
        }

        async fn call_callback(
            &self,
            _event: SlotEvent<usize, ()>,
            state: serde_json::Value,
            _completed: usize,
            _total: usize,
        ) -> anyhow::Result<SessionDecision<usize>> {
            let action = self
                .answers
                .lock()
                .unwrap()
                .pop()
                .expect("a scripted answer")?;
            Ok(SessionDecision {
                state,
                action,
                refill: true,
                cancel_ids: Vec::new(),
            })
        }

        async fn cancel_active(&self, _active: &[(usize, String)]) {}

        async fn cancel_selected(&self, _active: &mut Vec<(usize, String)>, _ids: &HashSet<usize>) {
        }

        async fn on_end(&self, end: SessionEnd) {
            self.ends.lock().unwrap().push(match end {
                SessionEnd::Decided(action) => format!("decided {action:?}"),
                SessionEnd::Aborted(_) => "aborted".into(),
                SessionEnd::Interrupted => "interrupted".into(),
            });
        }
    }

    async fn ends_of(items: usize, answers: Vec<anyhow::Result<SessionAction>>) -> Vec<String> {
        let driver = Scripted::new(answers);
        let ends = driver.ends.clone();
        run_windowed_session(
            driver,
            vec![(); items],
            1,
            Duration::from_secs(1),
            serde_json::Value::Null,
        )
        .await;
        ends.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_failed_result_callback_aborts_the_session_once() {
        let ends = ends_of(
            3,
            vec![Ok(SessionAction::Continue), Err(anyhow::anyhow!("gone"))],
        )
        .await;
        assert_eq!(ends, ["aborted"]);
    }

    #[tokio::test]
    async fn a_failed_exhausted_callback_aborts_the_session() {
        let ends = ends_of(
            1,
            vec![Ok(SessionAction::Continue), Err(anyhow::anyhow!("gone"))],
        )
        .await;
        assert_eq!(ends, ["aborted"]);
        let ends = ends_of(0, vec![Err(anyhow::anyhow!("gone"))]).await;
        assert_eq!(ends, ["aborted"]);
    }

    #[tokio::test]
    async fn a_callback_cut_off_by_shutdown_interrupts_the_session() {
        let shutdown = anyhow::Error::from(plugin_core::error::PluginError::ShuttingDown)
            .context("Detached evaluate callback failed");
        let ends = ends_of(2, vec![Err(shutdown)]).await;
        assert_eq!(ends, ["interrupted"]);
    }

    #[tokio::test]
    async fn the_plugin_decision_ends_the_session() {
        let ends = ends_of(3, vec![Ok(SessionAction::Finish)]).await;
        assert_eq!(ends, ["decided Finish"]);
        let ends = ends_of(
            1,
            vec![Ok(SessionAction::Continue), Ok(SessionAction::Continue)],
        )
        .await;
        assert_eq!(ends, ["decided Continue"]);
    }

    #[test]
    fn remove_active_slot_removes_by_batch_id_then_ignores_late_duplicate() {
        let mut active = vec![(0usize, "batch-1".to_string()), (1, "batch-2".to_string())];
        // A delivered result for an active slot is removed once...
        assert!(remove_active_slot(&mut active, "batch-1"));
        // ...and a late/duplicate result for the same (now-removed) slot is ignored.
        assert!(!remove_active_slot(&mut active, "batch-1"));
        assert_eq!(active, vec![(1, "batch-2".to_string())]);
    }
}
