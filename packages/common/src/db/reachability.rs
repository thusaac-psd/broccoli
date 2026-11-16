//! What one pool knows about whether the database can be reached, so a
//! statement fails at once while it cannot instead of each waiting out the
//! pool's acquire timeout.
//!
//! A statement whose error is an outage ([`super::is_outage`]) marks the
//! database unreachable, and any other outcome marks it reachable. While it
//! is unreachable, one statement at a time is let through as the probe and
//! every other one is refused at once with an outage error, which callers
//! answer as retryable. The probe that reaches the database lets everything
//! through again. The state is this process's own; nothing compares it across
//! nodes.

use std::sync::atomic::{AtomicBool, Ordering};

use sea_orm::{ConnAcquireErr, DbErr};

#[derive(Debug, Default)]
pub(super) struct Reachability {
    unreachable: AtomicBool,
    probing: AtomicBool,
}

/// A statement let through; a probe holds the probe slot until it drops.
pub(super) struct Pass<'a>(Option<&'a Reachability>);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        if let Some(reach) = self.0 {
            reach.probing.store(false, Ordering::Release);
        }
    }
}

impl Reachability {
    /// Let a statement through, as the probe while the database is
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

    /// Record what a statement let through found.
    pub(super) fn settle<T>(&self, outcome: &Result<T, DbErr>) {
        let unreachable = matches!(outcome, Err(e) if super::is_outage(e));
        self.unreachable.store(unreachable, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outage() -> Result<(), DbErr> {
        Err(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
    }

    #[test]
    fn statements_pass_while_the_database_answers() {
        let reach = Reachability::default();
        for _ in 0..3 {
            let _pass = reach.pass().unwrap();
        }
        reach.settle(&Err::<(), _>(DbErr::RecordNotFound("row".into())));
        assert!(reach.pass().is_ok());
    }

    #[test]
    fn an_outage_lets_one_probe_through_and_refuses_the_rest() {
        let reach = Reachability::default();
        reach.settle(&outage());

        let probe = reach.pass().expect("the first statement probes");
        let refused = reach.pass().err().expect("the others are refused");
        assert!(super::super::is_outage(&refused));
        drop(probe);

        // A probe that finds the database still unreachable hands the slot on.
        let probe = reach.pass().unwrap();
        reach.settle(&outage());
        drop(probe);
        let probe = reach.pass().unwrap();
        assert!(reach.pass().is_err());

        // One that reaches it lets everything through again.
        reach.settle(&Ok(()));
        drop(probe);
        let _a = reach.pass().unwrap();
        let _b = reach.pass().unwrap();
    }
}
