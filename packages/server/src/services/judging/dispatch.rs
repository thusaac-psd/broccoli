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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use plugin_core::executor::CallLocals;

use super::fault::{DispatchFault, DispatchFaults};
use super::unit::Unit;

/// The unit a dispatch hands its plugin, at the epoch it was claimed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

impl DispatchedUnit {
    fn kind(self) -> Unit {
        match self {
            Self::Judgement { .. } => Unit::Judgement,
            Self::CodeRun { .. } => Unit::CodeRun,
        }
    }

    /// The row id and epoch the lease of this unit is refreshed by.
    fn lease(self) -> (i32, i32) {
        match self {
            Self::Judgement {
                judgement_id,
                judge_epoch,
                ..
            } => (judgement_id, judge_epoch),
            Self::CodeRun {
                code_run_id,
                judge_epoch,
            } => (code_run_id, judge_epoch),
        }
    }
}

/// The units this server works on: a unit is live from the moment its claim
/// takes it ([`LiveDispatches::enter`], before anything about its dispatch
/// can wait) until its [`LiveUnit`] is dropped, which the [`Dispatch`] built
/// from it holds until its last clone (the contest call and every detached
/// session it started hold one) is dropped. The lease refresh keeps exactly
/// these units' leases fresh, so a unit this server owns but no longer works
/// on (its task panicked or was lost) lapses and is stolen instead of being
/// kept alive by a server that never finishes it, and a unit whose dispatch
/// is still preparing or waiting for a dispatcher permit keeps its lease.
#[derive(Clone, Debug, Default)]
pub struct LiveDispatches(Arc<Mutex<HashMap<DispatchedUnit, usize>>>);

impl LiveDispatches {
    /// Makes `unit` live until the returned entry is dropped. `needs_loaded`
    /// says whether this replica had every plugin the unit needs loaded when
    /// it claimed it (see [`Dispatch::missing_plugin`]).
    pub fn enter(&self, unit: DispatchedUnit, needs_loaded: bool) -> LiveUnit {
        *self.units().entry(unit).or_default() += 1;
        LiveUnit {
            live: self.clone(),
            unit,
            needs_loaded,
        }
    }

    /// The ids and epochs of the live units of `kind`, as two parallel lists.
    pub(crate) fn leases(&self, kind: Unit) -> (Vec<i32>, Vec<i32>) {
        self.units()
            .keys()
            .filter(|unit| unit.kind() == kind)
            .map(|unit| unit.lease())
            .unzip()
    }

    fn units(&self) -> std::sync::MutexGuard<'_, HashMap<DispatchedUnit, usize>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A unit's place in [`LiveDispatches`]; dropping it leaves.
#[derive(Debug)]
pub struct LiveUnit {
    live: LiveDispatches,
    unit: DispatchedUnit,
    needs_loaded: bool,
}

impl LiveUnit {
    pub fn unit(&self) -> DispatchedUnit {
        self.unit
    }

    /// Whether every plugin the unit needs was loaded here when it was
    /// claimed.
    pub fn needs_loaded(&self) -> bool {
        self.needs_loaded
    }
}

impl Drop for LiveUnit {
    fn drop(&mut self) {
        let mut units = self.live.units();
        if let Some(count) = units.get_mut(&self.unit) {
            *count -= 1;
            if *count == 0 {
                units.remove(&self.unit);
            }
        }
    }
}

/// One dispatch: the plugin it went to, the unit it carries and the faults
/// its operations meet. Clones share the faults, and the unit stays live
/// ([`LiveDispatches`]) while any clone exists.
#[derive(Clone, Debug)]
pub struct Dispatch(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    owner: String,
    faults: DispatchFaults,
    live: LiveUnit,
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
    /// A dispatch of the live unit `live` to `owner`, recording into
    /// `faults`; the unit stays live until its last clone is dropped.
    pub fn new(owner: &str, live: LiveUnit, faults: DispatchFaults) -> Self {
        Self(Arc::new(Inner {
            owner: owner.to_string(),
            faults,
            live,
        }))
    }

    pub fn owner(&self) -> &str {
        &self.0.owner
    }

    pub fn unit(&self) -> DispatchedUnit {
        self.0.live.unit
    }

    /// The faults this dispatch's operations met.
    pub fn faults(&self) -> &DispatchFaults {
        &self.0.faults
    }

    /// Records that a statement this dispatch's plugins ran failed with
    /// `err` because the database did not answer (`common::db::is_outage`):
    /// the attempt is handed back ([`DispatchFault::DatabaseUnavailable`]).
    /// Any other error is the plugin's to handle and records nothing.
    pub fn note_database_error(&self, err: &sea_orm::DbErr) {
        if common::db::is_outage(err) {
            self.faults()
                .record(DispatchFault::DatabaseUnavailable, err.to_string());
        }
    }

    /// Records that a plugin the unit needs did not resolve here
    /// (`unresolved`), as `judging::fault::missing_plugin` decides: handed
    /// back when the unit was claimed with every plugin it needs loaded
    /// here (it is reloading or was unloaded since), standing when this
    /// replica took it while no live replica had the plugin.
    pub fn missing_plugin(&self, unresolved: &crate::providers::Unresolved) {
        let recorded = super::fault::missing_plugin(self.0.live.needs_loaded, unresolved);
        self.0.faults.record(recorded.fault, recorded.message);
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
        let locals = Dispatch::new(
            "p",
            LiveDispatches::default().enter(UNIT, true),
            faults.clone(),
        )
        .call_locals();
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
        let dispatch = || {
            Dispatch::new(
                "p",
                LiveDispatches::default().enter(UNIT, true),
                DispatchFaults::default(),
            )
        };
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

    /// A plugin missing mid-dispatch is recorded as this replica's fault,
    /// losing the attempt, for a unit claimed with every plugin loaded. A
    /// unit taken while no live replica had the plugin loaded stands on its
    /// first attempt, naming the plugin and its role.
    #[test]
    fn a_missing_plugin_hands_back_what_was_claimed_loaded_and_stands_otherwise() {
        use super::super::fault::SystemErrorClass;
        use broccoli_api_types::error::HostCode;
        for needs_loaded in [true, false] {
            let faults = DispatchFaults::default();
            let dispatch = Dispatch::new(
                "p",
                LiveDispatches::default().enter(UNIT, needs_loaded),
                faults.clone(),
            );
            let unresolved = crate::providers::Unresolved {
                kind: crate::providers::Provided::CheckerFormat,
                key: "lines".into(),
                provider: Some("standard-checkers".into()),
            };
            dispatch.missing_plugin(&unresolved);
            let recorded = faults.get().expect("recorded");
            if needs_loaded {
                assert_eq!(recorded.fault, DispatchFault::NotLoadedHere);
                assert!(faults.lost().is_some());
            } else {
                assert_eq!(recorded.fault, DispatchFault::NotLoaded);
                assert_eq!(
                    SystemErrorClass::of_fault(recorded.fault),
                    SystemErrorClass::Stands
                );
                assert_eq!(recorded.fault.code(), HostCode::PluginNotLoaded);
                assert_eq!(
                    recorded.message,
                    "the checker plugin standard-checkers for checker format \"lines\" \
                     is loaded on no live replica"
                );
            }
        }
    }

    /// A database outage a dispatch's plugin meets loses the attempt; any
    /// other statement error is the plugin's own.
    #[test]
    fn a_database_outage_loses_the_attempt() {
        let faults = DispatchFaults::default();
        let dispatch = Dispatch::new(
            "p",
            LiveDispatches::default().enter(UNIT, true),
            faults.clone(),
        );
        dispatch.note_database_error(&sea_orm::DbErr::Custom("duplicate key".into()));
        assert!(faults.get().is_none(), "a plugin's own error");
        dispatch.note_database_error(&sea_orm::DbErr::ConnectionAcquire(
            sea_orm::error::ConnAcquireErr::Timeout,
        ));
        assert_eq!(
            faults.lost().map(|r| r.fault),
            Some(DispatchFault::DatabaseUnavailable)
        );
    }
}
