//! What one pool knows about whether the database can be reached, so a
//! statement fails at once while it cannot instead of each waiting out the
//! pool's acquire timeout.
//!
//! Only evidence that the database cannot be reached marks it unreachable: a
//! statement or connection that fails as an outage ([`super::is_outage`]),
//! or an acquire that timed out while nothing showed the database answering.
//! An acquire that timed out while the pool was busy is saturation, not an
//! outage: a transaction held one of its connections, or a statement got an
//! answer, while it waited. That request still answers as unavailable, but
//! the others keep waiting for connections as before. Any answer marks the
//! database reachable.
//!
//! While it is unreachable, one statement at a time is let through as the
//! probe and every other one is refused at once with an outage error, which
//! callers answer as retryable. The state is this process's own; nothing
//! compares it across nodes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

#[derive(Debug, Default)]
pub(super) struct Reachability {
    unreachable: AtomicBool,
    probing: AtomicBool,
    /// Answers the database gave on this pool, ever.
    answers: AtomicU64,
    /// Transactions open on this pool, each holding a connection that
    /// answered when it began.
    held: AtomicUsize,
}

/// A statement let through: the answers counted when it started, and, for a
/// probe, the probe slot until it drops.
pub(super) struct Pass<'a> {
    reach: &'a Reachability,
    answers_at_start: u64,
    probe: bool,
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        if self.probe {
            self.reach.probing.store(false, Ordering::Release);
        }
    }
}

/// A connection an open transaction holds, counted in [`Reachability`] until
/// the transaction ends.
#[derive(Debug)]
pub(super) struct Held(Arc<Reachability>);

impl Held {
    pub(super) fn reachability(&self) -> &Arc<Reachability> {
        &self.0
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.held.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Reachability {
    /// Let a statement through, as the probe while the database is
    /// unreachable, or refuse it with the outage error to answer instead.
    pub(super) fn pass(&self) -> Result<Pass<'_>, DbErr> {
        let answers_at_start = self.answers.load(Ordering::Acquire);
        if !self.unreachable.load(Ordering::Acquire) {
            return Ok(Pass {
                reach: self,
                answers_at_start,
                probe: false,
            });
        }
        self.probing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Pass {
                reach: self,
                answers_at_start,
                probe: true,
            })
            .map_err(|_| DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
    }

    /// Record what a statement let through with `pass` found.
    pub(super) fn settle<T>(&self, pass: &Pass<'_>, outcome: &Result<T, DbErr>) {
        match outcome {
            Err(e) if super::is_outage(e) => {
                let saturated = is_acquire_timeout(e)
                    && (self.held.load(Ordering::Acquire) > 0
                        || self.answers.load(Ordering::Acquire) != pass.answers_at_start);
                if !saturated {
                    self.unreachable.store(true, Ordering::Release);
                }
            }
            _ => self.answered(),
        }
    }

    /// Record an answer from the database.
    pub(super) fn answered(&self) {
        self.answers.fetch_add(1, Ordering::AcqRel);
        self.unreachable.store(false, Ordering::Release);
    }

    /// Count a connection a transaction that began holds until it ends.
    pub(super) fn hold(self: &Arc<Self>) -> Held {
        self.held.fetch_add(1, Ordering::AcqRel);
        Held(self.clone())
    }
}

/// Whether `e` says no pooled connection came free in time, which a busy
/// pool answers as well as an unreachable database.
fn is_acquire_timeout(e: &DbErr) -> bool {
    use sea_orm::sqlx::Error;
    match e {
        DbErr::ConnectionAcquire(ConnAcquireErr::Timeout) => true,
        DbErr::Conn(RuntimeErr::SqlxError(e))
        | DbErr::Exec(RuntimeErr::SqlxError(e))
        | DbErr::Query(RuntimeErr::SqlxError(e)) => matches!(**e, Error::PoolTimedOut),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeout() -> Result<(), DbErr> {
        Err(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
    }

    fn broken() -> Result<(), DbErr> {
        Err(DbErr::Conn(RuntimeErr::SqlxError(
            sea_orm::sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
                .into(),
        )))
    }

    /// Run one statement through `reach` that ends with `outcome`.
    fn run(reach: &Reachability, outcome: Result<(), DbErr>) {
        let pass = reach.pass().unwrap();
        reach.settle(&pass, &outcome);
    }

    #[test]
    fn statements_pass_while_the_database_answers() {
        let reach = Reachability::default();
        for _ in 0..3 {
            let _pass = reach.pass().unwrap();
        }
        run(&reach, Err(DbErr::RecordNotFound("row".into())));
        assert!(reach.pass().is_ok());
    }

    #[test]
    fn an_outage_lets_one_probe_through_and_refuses_the_rest() {
        let reach = Reachability::default();
        run(&reach, broken());

        let probe = reach.pass().expect("the first statement probes");
        let refused = reach.pass().err().expect("the others are refused");
        assert!(super::super::is_outage(&refused));

        // A probe that finds the database still unreachable hands the slot on.
        reach.settle(&probe, &timeout());
        drop(probe);
        let probe = reach.pass().unwrap();
        assert!(reach.pass().is_err());

        // One that reaches it lets everything through again.
        reach.settle(&probe, &Ok(()));
        drop(probe);
        let _a = reach.pass().unwrap();
        let _b = reach.pass().unwrap();
    }

    #[test]
    fn an_acquire_timeout_with_nothing_answering_is_an_outage() {
        let reach = Reachability::default();
        run(&reach, timeout());
        let _probe = reach.pass().unwrap();
        assert!(reach.pass().is_err());
    }

    #[test]
    fn an_acquire_timeout_while_a_transaction_holds_a_connection_is_saturation() {
        let reach = Arc::new(Reachability::default());
        let held = reach.hold();
        run(&reach, timeout());
        let _a = reach.pass().unwrap();
        let _b = reach.pass().unwrap();
        drop((_a, _b, held));

        // A transport failure is an outage however busy the pool is.
        let _held = reach.hold();
        run(&reach, broken());
        let _probe = reach.pass().unwrap();
        assert!(reach.pass().is_err());
    }

    #[test]
    fn an_acquire_timeout_while_other_statements_were_answered_is_saturation() {
        let reach = Reachability::default();
        let waiting = reach.pass().unwrap();
        run(&reach, Ok(()));
        reach.settle(&waiting, &timeout());
        drop(waiting);
        let _a = reach.pass().unwrap();
        let _b = reach.pass().unwrap();
    }
}
