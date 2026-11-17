//! What one pool knows about whether the database can be reached, so a
//! statement fails at once while it cannot instead of each waiting out the
//! pool's acquire timeout.
//!
//! The pool is busy while any of its connections is checked out: a standalone
//! statement running, a transaction open, or a connection a caller holds
//! ([`Held`] counts each). Every checkout goes through the handle, so the
//! count is exact. Two rules decide what an outage error means:
//!
//! - An acquire that times out while the pool is busy is saturation: every
//!   connection is in use, so that request answers as unavailable and nothing
//!   else changes. An acquire that times out with no connection checked out
//!   could not open one, and marks the database unreachable, as any other
//!   outage error of an acquire does.
//! - A statement that fails as an outage on a connection it holds
//!   ([`super::is_outage`]: the connection broke, the server is shutting down)
//!   marks the database unreachable, however busy the pool is.
//!
//! Any other outcome is an answer and marks the database reachable.
//!
//! While it is unreachable, one acquire at a time is let through as the probe
//! and every other one is refused at once with an outage error, which callers
//! answer as retryable. The state is this process's own; nothing compares it
//! across nodes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

#[derive(Debug, Default)]
pub(super) struct Reachability {
    unreachable: AtomicBool,
    probing: AtomicBool,
    /// Connections checked out of the pool.
    busy: AtomicUsize,
}

/// An acquire let through; a probe holds the probe slot until it drops.
pub(super) struct Pass<'a>(Option<&'a Reachability>);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        if let Some(reach) = self.0 {
            reach.probing.store(false, Ordering::Release);
        }
    }
}

/// A checked-out connection, counted busy in [`Reachability`] until it is
/// returned.
#[derive(Debug)]
pub(super) struct Held(Arc<Reachability>);

impl Held {
    /// Another count of the same pool, for a nested transaction that shares
    /// this connection: the connection stays busy until both are done.
    pub(super) fn share(&self) -> Held {
        self.0.busy.fetch_add(1, Ordering::AcqRel);
        Held(self.0.clone())
    }

    /// Record what a statement on this connection found.
    pub(super) fn settle<T>(&self, outcome: &Result<T, DbErr>) {
        match outcome {
            Err(e) if super::is_outage(e) => self.0.lost(),
            _ => self.0.answered(),
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Reachability {
    /// Let an acquire through, as the probe while the database is
    /// unreachable, or refuse it with the outage error to answer instead.
    pub(super) fn pass(&self) -> Result<Pass<'_>, DbErr> {
        if !self.unreachable.load(Ordering::Acquire) {
            return Ok(Pass(None));
        }
        self.probing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Pass(Some(self)))
            .map_err(|_| DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
    }

    /// Record what an acquire let through found, and count the connection it
    /// checked out busy until the returned [`Held`] drops.
    pub(super) fn settle_acquire<T>(
        self: &Arc<Self>,
        _pass: Pass<'_>,
        outcome: Result<T, DbErr>,
    ) -> Result<(T, Held), DbErr> {
        match outcome {
            Ok(conn) => {
                self.answered();
                self.busy.fetch_add(1, Ordering::AcqRel);
                Ok((conn, Held(self.clone())))
            }
            Err(e) => {
                let saturated = is_acquire_timeout(&e) && self.busy.load(Ordering::Acquire) > 0;
                if super::is_outage(&e) && !saturated {
                    self.lost();
                } else if !super::is_outage(&e) {
                    self.answered();
                }
                Err(e)
            }
        }
    }

    fn answered(&self) {
        self.unreachable.store(false, Ordering::Release);
    }

    fn lost(&self) {
        self.unreachable.store(true, Ordering::Release);
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

    /// Run one acquire through `reach` that ends with `outcome`, returning the
    /// connection it checked out, if any.
    fn acquire(reach: &Arc<Reachability>, outcome: Result<(), DbErr>) -> Option<Held> {
        let pass = reach.pass().unwrap();
        reach
            .settle_acquire(pass, outcome)
            .ok()
            .map(|(_, held)| held)
    }

    fn refused(reach: &Reachability) -> bool {
        reach.pass().is_err()
    }

    #[test]
    fn acquires_pass_while_the_database_answers() {
        let reach = Arc::new(Reachability::default());
        let _a = acquire(&reach, Ok(())).unwrap();
        let _b = acquire(&reach, Ok(())).unwrap();
        _a.settle(&Err::<(), _>(DbErr::RecordNotFound("row".into())));
        assert!(!refused(&reach));
    }

    #[test]
    fn an_outage_lets_one_probe_through_and_refuses_the_rest() {
        let reach = Arc::new(Reachability::default());
        acquire(&reach, broken());

        let probe = reach.pass().expect("the first acquire probes");
        let err = reach.pass().err().expect("the others are refused");
        assert!(super::super::is_outage(&err));

        // A probe that finds the database still unreachable hands the slot on.
        let _ = reach.settle_acquire(probe, timeout());
        let probe = reach.pass().unwrap();
        assert!(refused(&reach));

        // One that reaches it lets everything through again.
        let held = reach.settle_acquire(probe, Ok(())).unwrap().1;
        assert!(!refused(&reach) && !refused(&reach));
        drop(held);
    }

    #[test]
    fn an_acquire_timeout_with_nothing_checked_out_is_an_outage() {
        let reach = Arc::new(Reachability::default());
        acquire(&reach, timeout());
        let _probe = reach.pass().unwrap();
        assert!(refused(&reach));
    }

    #[test]
    fn an_acquire_timeout_while_a_connection_is_checked_out_is_saturation() {
        let reach = Arc::new(Reachability::default());
        let running = acquire(&reach, Ok(())).unwrap();
        for _ in 0..3 {
            acquire(&reach, timeout());
            assert!(!refused(&reach), "a busy pool is not a lost database");
        }
        drop(running);
        acquire(&reach, timeout());
        let _probe = reach.pass().expect("one acquire probes");
        assert!(refused(&reach), "with nothing checked out, it is lost");
    }

    #[test]
    fn a_broken_connection_is_an_outage_however_busy_the_pool_is() {
        let reach = Arc::new(Reachability::default());
        let busy = acquire(&reach, Ok(())).unwrap();
        let other = acquire(&reach, Ok(())).unwrap();
        other.settle(&broken());
        let _probe = reach.pass().unwrap();
        assert!(refused(&reach));
        drop(busy);
    }
}
