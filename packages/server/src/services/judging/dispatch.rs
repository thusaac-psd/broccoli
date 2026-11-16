//! The dispatch a plugin call works for.
//!
//! The host hands a judgement or code run to the plugin that provides its
//! contest type ([`Dispatch`]). That plugin call, the calls nested in it and
//! the detached callbacks it starts carry the dispatch in their call locals,
//! and every plugin write to a judgement or code run is checked against it
//! ([`Dispatch::owned`]): a plugin writes only the unit its own call was
//! dispatched with. A write outside a dispatch, from another plugin of the
//! call tree (a checker or an evaluator), or naming another unit is refused
//! before it reaches the database. The database fence then refuses a write of
//! a dispatch that has since lost its unit (see [`super::write_fence`]).

use std::sync::Arc;

use plugin_core::executor::CallLocals;

use super::fault::DispatchFaults;

/// The unit a dispatch hands its plugin, at the epoch it was claimed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchedUnit {
    Judgement {
        submission_id: i32,
        judgement_id: i32,
        judge_epoch: i32,
    },
    CodeRun {
        code_run_id: i32,
        judge_epoch: i32,
    },
}

/// One dispatch: the plugin it went to, the unit it carries and the faults
/// its operations meet. Clones share the faults.
#[derive(Clone, Debug)]
pub struct Dispatch(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    owner: String,
    unit: DispatchedUnit,
    faults: DispatchFaults,
}

/// The [`Dispatch`] a call tree's [`CallLocals`] hold.
#[derive(Default)]
struct CallDispatch(Option<Dispatch>);

/// Why a plugin write was refused before it reached the database.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotOwned {
    #[error("this call was not dispatched with a judgement or code run")]
    NoDispatch,
    #[error("this call's dispatch belongs to another plugin")]
    OtherPlugin,
    #[error("this call was dispatched with another judgement or code run")]
    OtherUnit,
}

impl Dispatch {
    /// A dispatch of `unit` to `owner`, recording into `faults`.
    pub fn new(owner: &str, unit: DispatchedUnit, faults: DispatchFaults) -> Self {
        Self(Arc::new(Inner {
            owner: owner.to_string(),
            unit,
            faults,
        }))
    }

    pub fn owner(&self) -> &str {
        &self.0.owner
    }

    pub fn unit(&self) -> DispatchedUnit {
        self.0.unit
    }

    /// The faults this dispatch's operations met.
    pub fn faults(&self) -> &DispatchFaults {
        &self.0.faults
    }

    /// Fresh call locals for a plugin call made for this dispatch.
    pub fn call_locals(&self) -> Arc<CallLocals> {
        let locals: Arc<CallLocals> = Arc::default();
        locals.with(|d: &mut CallDispatch| d.0 = Some(self.clone()));
        locals
    }

    /// The dispatch of the plugin call tree running on this thread, if any.
    pub fn of_current_call() -> Option<Self> {
        plugin_core::executor::call_locals().and_then(|locals| Self::of(&locals))
    }

    fn of(locals: &CallLocals) -> Option<Self> {
        locals.with(|d: &mut CallDispatch| d.0.clone())
    }

    /// The running call's dispatch, when `plugin_id` owns it and it carries
    /// `unit`: the check every plugin write to a judgement or code run passes.
    pub(crate) fn owned(plugin_id: &str, unit: DispatchedUnit) -> Result<Self, NotOwned> {
        Self::of_current_call()
            .ok_or(NotOwned::NoDispatch)?
            .admits(plugin_id, unit)
    }

    /// This dispatch, when `plugin_id` owns it and it carries `unit`.
    pub(crate) fn admits(self, plugin_id: &str, unit: DispatchedUnit) -> Result<Self, NotOwned> {
        if self.owner() != plugin_id {
            return Err(NotOwned::OtherPlugin);
        }
        if self.unit() != unit {
            return Err(NotOwned::OtherUnit);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::worker::OperationFault;

    const UNIT: DispatchedUnit = DispatchedUnit::Judgement {
        submission_id: 1,
        judgement_id: 2,
        judge_epoch: 3,
    };

    #[test]
    fn a_seeded_call_shares_the_dispatchs_faults() {
        let faults = DispatchFaults::default();
        let locals = Dispatch::new("p", UNIT, faults.clone()).call_locals();
        Dispatch::of(&locals)
            .expect("seeded")
            .faults()
            .record(OperationFault::OperationLost, "lost");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::OperationLost.into())
        );
    }

    #[test]
    fn only_the_owner_writes_and_only_its_unit() {
        let dispatch = || Dispatch::new("p", UNIT, DispatchFaults::default());
        assert!(dispatch().admits("p", UNIT).is_ok());
        assert_eq!(
            dispatch().admits("checker", UNIT).unwrap_err(),
            NotOwned::OtherPlugin
        );
        for other in [
            DispatchedUnit::Judgement {
                submission_id: 1,
                judgement_id: 9,
                judge_epoch: 3,
            },
            DispatchedUnit::Judgement {
                submission_id: 1,
                judgement_id: 2,
                judge_epoch: 4,
            },
            DispatchedUnit::CodeRun {
                code_run_id: 2,
                judge_epoch: 3,
            },
        ] {
            assert_eq!(
                dispatch().admits("p", other).unwrap_err(),
                NotOwned::OtherUnit
            );
        }
        assert!(Dispatch::of(&CallLocals::default()).is_none());
    }
}
