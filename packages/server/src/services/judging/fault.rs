//! Faults under a dispatch, and what the system-error retry does with the
//! judgement they end.
//!
//! Everything that works for one dispatch (its plugin calls, the operations
//! they publish, the evaluate batches and detached sessions they start)
//! shares one [`DispatchFaults`]. Whatever observes a fault records it there:
//! a publish the queue refused, an operation the queue lost, a worker without
//! a platform tool, a problem's checker that crashed. When the dispatch then
//! ends its judgement as a system error, the judgement carries the fault's
//! code ([`DispatchFault::code`]) and description instead of whatever the
//! plugin reported, and [`SystemErrorClass::of_fault`] decides what becomes
//! of it. A plugin never classifies a fault: the most it says is that a
//! system error it writes stands (`SubmissionUpdate::stands`).

use std::sync::{Arc, Mutex};

use broccoli_api_types::error::HostCode;
use common::worker::OperationFault;

/// A fault one dispatch met that decides its system error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchFault {
    /// The platform lost one of its operations.
    Operation(OperationFault),
    /// A plugin the unit needs, loaded on this replica when it claimed the
    /// unit, is not loaded here any more (it is reloading or was unloaded):
    /// the replica's fault, not the submission's.
    NotLoadedHere,
    /// No enabled plugin provides a key the unit needs, and this replica
    /// took it without one loaded: no replica will run it, so the failure
    /// stands.
    NotLoaded,
    /// The database did not answer a statement the dispatch's plugins ran
    /// (`common::db::is_outage`): the platform's fault, not the submission's.
    /// A busy pool and a lost database are one fault, since both hand back
    /// without spending a retry; only a client's answer tells them apart.
    DatabaseUnavailable,
    /// The problem's checker program failed (crashed, was killed by a
    /// signal, exited with a code its format gives no meaning), as its
    /// checker plugin reported through `CheckerVerdict::checker_failure`.
    CheckerFailed,
    /// Another program the problem supplies (an interactor or a manager)
    /// failed, as its evaluator plugin reported through
    /// `TestCaseVerdict::problem_failure`.
    ProblemFailed,
}

impl DispatchFault {
    /// The error code a judgement this fault ends carries.
    pub fn code(self) -> HostCode {
        match self {
            Self::Operation(fault) => fault.code(),
            Self::NotLoadedHere => HostCode::PluginNotLoadedHere,
            Self::NotLoaded => HostCode::NoProvider,
            Self::DatabaseUnavailable => HostCode::DatabaseUnavailable,
            Self::CheckerFailed => HostCode::CheckerFailed,
            Self::ProblemFailed => HostCode::ProblemFailed,
        }
    }
}

impl From<OperationFault> for DispatchFault {
    fn from(fault: OperationFault) -> Self {
        Self::Operation(fault)
    }
}

/// The fault that decides a dispatch's system error, if any.
#[derive(Clone, Default, Debug)]
pub struct DispatchFaults(Arc<Mutex<Option<Recorded>>>);

/// A recorded fault with the operator-facing description of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub fault: DispatchFault,
    pub message: String,
}

impl DispatchFaults {
    /// Record `fault`. The first one recorded stays, except that a lost
    /// attempt replaces any other: nothing the attempt found stands once the
    /// platform lost it.
    pub fn record(&self, fault: impl Into<DispatchFault>, message: impl Into<String>) {
        let fault = fault.into();
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let replaces = match slot.as_ref() {
            None => true,
            Some(held) => loses(fault) && !loses(held.fault),
        };
        if replaces {
            *slot = Some(Recorded {
                fault,
                message: message.into(),
            });
        }
    }

    /// The fault that decides this dispatch's system error.
    pub fn get(&self) -> Option<Recorded> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The fault that lost this dispatch's attempt, if one did: a fault of
    /// `SystemErrorClass::HandBack`. A lost attempt never becomes a
    /// verdict: its judging session ends without asking the plugin, a
    /// plugin write that would finalize it is fenced out, and the dispatch
    /// hands the judgement back (`fail_dispatch`).
    pub fn lost(&self) -> Option<Recorded> {
        self.get().filter(|r| loses(r.fault))
    }
}

/// The fault of a plugin a unit needs that did not resolve here
/// (`unresolved`). A key an enabled plugin provides is this replica's to
/// lose: it is reloading or was unloaded since the claim, or another replica
/// has it, so the attempt is handed back ([`DispatchFault::NotLoadedHere`])
/// and the unit waits, parked while its provider is loaded nowhere, for a
/// replica that runs it. Only a key no enabled plugin provides, taken by a
/// replica that did not have it loaded either, stands on its first attempt
/// ([`DispatchFault::NotLoaded`]): no replica will ever run it.
pub(crate) fn missing_plugin(
    needs_loaded: bool,
    unresolved: &crate::providers::Unresolved,
) -> Recorded {
    if needs_loaded || unresolved.provider.is_some() {
        Recorded {
            fault: DispatchFault::NotLoadedHere,
            message: unresolved.to_string(),
        }
    } else {
        Recorded {
            fault: DispatchFault::NotLoaded,
            message: unresolved.to_string(),
        }
    }
}

/// Whether `fault` loses the attempt it meets.
fn loses(fault: DispatchFault) -> bool {
    SystemErrorClass::of_fault(fault) == SystemErrorClass::HandBack
}

/// What becomes of a unit whose attempt ended in a system error. The class is
/// decided where the failure is observed (a dispatch fault, the dispatcher's
/// own failure, a plugin's write) and recorded with it, never read back from
/// the error code, so no code a plugin writes can pass for another class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemErrorClass {
    /// A failure judging the submission: retried while its submission's
    /// system-error budget lasts.
    Judging,
    /// The platform lost the attempt (the queue was unreachable or lost an
    /// operation, the worker that took it lacks a platform tool, no worker
    /// could take it, a plugin it needs stopped being loaded on the replica
    /// that claimed it, or the database did not answer): handed back to the queue at once, spending no budget,
    /// and never finalized (see [`DispatchFaults::lost`]). A worker without
    /// its platform tools takes no work, so the next attempt goes to one that
    /// has them.
    HandBack,
    /// A failure every attempt repeats: the contest plugin refused the
    /// submission or found the problem cannot be judged, or the problem's
    /// checker fails. It stands at once (`error_stands`).
    Stands,
}

impl SystemErrorClass {
    pub(crate) fn of_fault(fault: DispatchFault) -> Self {
        match fault {
            DispatchFault::Operation(
                OperationFault::QueueUnreachable
                | OperationFault::OperationLost
                | OperationFault::PlatformToolMissing
                | OperationFault::NoCapableWorker,
            )
            | DispatchFault::NotLoadedHere
            | DispatchFault::DatabaseUnavailable => Self::HandBack,
            DispatchFault::NotLoaded
            | DispatchFault::CheckerFailed
            | DispatchFault::ProblemFailed => Self::Stands,
        }
    }
}

/// Why a dispatch ended its unit without a verdict: the code and message the
/// unit records, and the class that decides what becomes of it
/// ([`super::fail_dispatch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DispatchFailure {
    pub code: HostCode,
    pub message: String,
    pub class: SystemErrorClass,
}

impl DispatchFailure {
    /// A failure judging the submission, retried within its budget.
    pub(crate) fn judging(code: HostCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            class: SystemErrorClass::Judging,
        }
    }

    /// The contest plugin refused the work as one every attempt would fail.
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self {
            code: HostCode::PluginRefused,
            message: message.into(),
            class: SystemErrorClass::Stands,
        }
    }

    /// This failure, unless the dispatch met a fault: then the fault decides,
    /// whatever the dispatch saw of it.
    pub(crate) fn unless_fault(self, faults: &DispatchFaults) -> Self {
        faults.get().map_or(self, Self::from)
    }
}

/// How the plugin call of a dispatch (a judgement's or a code run's) ended,
/// the one rule both dispatchers read it by.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CallEnd {
    /// The plugin handled the unit; what it wrote stands.
    Handled,
    /// The unit ends with this failure (see [`super::fail_dispatch`]).
    Failed(DispatchFailure),
    /// This server is shutting down and refused the call. Not a failure of
    /// the work: the unit stays in flight, and shutdown releases it to a
    /// peer.
    ShuttingDown,
}

/// The output of a dispatch's plugin call.
pub(crate) trait DispatchOutput: serde::de::DeserializeOwned {
    /// The failure the plugin reported, if it reported one.
    fn failure(self) -> Option<DispatchFailure>;
}

impl DispatchOutput for broccoli_server_sdk::types::OnSubmissionOutput {
    fn failure(self) -> Option<DispatchFailure> {
        (!self.success).then(|| {
            let message = self
                .error_message
                .unwrap_or_else(|| "Unknown plugin error".to_string());
            if self.retryable {
                DispatchFailure::judging(HostCode::PluginError, message)
            } else {
                DispatchFailure::refused(message)
            }
        })
    }
}

impl DispatchOutput for broccoli_server_sdk::types::OnCodeRunOutput {
    fn failure(self) -> Option<DispatchFailure> {
        (!self.success).then(|| {
            DispatchFailure::judging(
                HostCode::PluginError,
                self.error_message
                    .unwrap_or_else(|| "Unknown plugin error".to_string()),
            )
        })
    }
}

impl CallEnd {
    /// How a dispatch's plugin call that returned `result` ended, read as
    /// output `O`. A call that could not finish because the host could not
    /// serve it (its deadline, admission, the plugin between loads, an
    /// instance it could not build; see `PluginError::is_unavailable`) says
    /// nothing about the work and ends as `PLUGIN_UNAVAILABLE`; any other
    /// error as `PLUGIN_EXECUTION_ERROR`. Both are judging failures, retried
    /// within the budget, so a unit that always outlasts its deadline still
    /// ends. A call that succeeded while the platform lost its attempt
    /// ([`DispatchFaults::lost`]) fails, so the fault hands it back.
    pub(crate) fn of<O: DispatchOutput>(
        result: Result<Vec<u8>, plugin_core::error::PluginError>,
        faults: &DispatchFaults,
    ) -> Self {
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(plugin_core::error::PluginError::ShuttingDown) => return Self::ShuttingDown,
            // Its plugin was loaded when the unit was claimed, since the
            // handler was registered, and is not now.
            Err(e @ plugin_core::error::PluginError::NotLoaded(_)) => {
                return Self::Failed(DispatchFailure::from(Recorded {
                    fault: DispatchFault::NotLoadedHere,
                    message: e.to_string(),
                }));
            }
            Err(e) if e.is_unavailable() => {
                return Self::Failed(DispatchFailure::judging(
                    HostCode::PluginUnavailable,
                    e.to_string(),
                ));
            }
            Err(e) => {
                return Self::Failed(DispatchFailure::judging(
                    HostCode::PluginExecutionError,
                    e.to_string(),
                ));
            }
        };
        let output = match serde_json::from_slice::<O>(&bytes) {
            Ok(output) => output,
            Err(e) => {
                return Self::Failed(DispatchFailure::judging(
                    HostCode::PluginInvalidOutput,
                    format!("Plugin returned invalid output: {e}"),
                ));
            }
        };
        match output.failure() {
            Some(failure) => Self::Failed(failure),
            // The plugin's verdict, if it wrote one, was fenced out.
            None if faults.lost().is_some() => Self::Failed(DispatchFailure::judging(
                HostCode::PluginError,
                "The platform lost the attempt",
            )),
            None => Self::Handled,
        }
    }
}

impl DispatchFailure {
    /// The dispatch met `fault` (described by `message`): it decides.
    pub(crate) fn faulted(fault: DispatchFault, message: String) -> Self {
        Self::from(Recorded { fault, message })
    }

    /// The dispatch's contest plugin did not resolve here (`unresolved`):
    /// the fault [`missing_plugin`] names.
    pub(crate) fn no_handler(
        needs_loaded: bool,
        unresolved: &crate::providers::Unresolved,
    ) -> Self {
        Self::from(missing_plugin(needs_loaded, unresolved))
    }
}

impl From<Recorded> for DispatchFailure {
    fn from(recorded: Recorded) -> Self {
        Self {
            code: recorded.fault.code(),
            message: recorded.message,
            class: SystemErrorClass::of_fault(recorded.fault),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_fault_decides_the_failure() {
        let faults = DispatchFaults::default();
        let own = DispatchFailure::judging(HostCode::PluginError, "boom");
        assert_eq!(own.clone().unless_fault(&faults), own);
        faults.record(DispatchFault::CheckerFailed, "signal 6");
        let failure = own.unless_fault(&faults);
        assert_eq!(failure.code, HostCode::CheckerFailed);
        assert_eq!(failure.message, "signal 6");
        assert_eq!(failure.class, SystemErrorClass::Stands);
    }

    fn end(result: Result<Vec<u8>, plugin_core::error::PluginError>) -> CallEnd {
        CallEnd::of::<broccoli_server_sdk::types::OnSubmissionOutput>(
            result,
            &DispatchFaults::default(),
        )
    }

    fn code(end: CallEnd) -> HostCode {
        match end {
            CallEnd::Failed(failure) => failure.code,
            other => panic!("{other:?}"),
        }
    }

    /// Every way a plugin call fails maps to one outcome, the same for a
    /// judgement and a code run: a call the host could not serve is
    /// unavailable, whatever made it so, except a plugin that is not loaded
    /// here any more, which is this replica's fault and hands the unit back.
    #[test]
    fn a_plugin_call_error_maps_to_one_dispatch_outcome() {
        use plugin_core::error::PluginError;
        let unavailable = [
            PluginError::Unavailable {
                plugin_id: "p".into(),
                func_name: "f".into(),
            },
            PluginError::InstanceBuild {
                plugin_id: "p".into(),
                message: "oom".into(),
            },
        ];
        for e in unavailable {
            assert_eq!(code(end(Err(e))), HostCode::PluginUnavailable);
        }
        let CallEnd::Failed(unloaded) = end(Err(PluginError::NotLoaded("p".into()))) else {
            panic!("failed");
        };
        assert_eq!(unloaded.code, HostCode::PluginNotLoadedHere);
        assert_eq!(unloaded.class, SystemErrorClass::HandBack);
        let failed = PluginError::ExecutionFailed {
            plugin_id: "p".into(),
            func_name: "f".into(),
            depth: 1,
            message: "trap".into(),
        };
        assert_eq!(code(end(Err(failed))), HostCode::PluginExecutionError);
        assert_eq!(end(Err(PluginError::ShuttingDown)), CallEnd::ShuttingDown);
        assert_eq!(
            code(end(Ok(b"nope".to_vec()))),
            HostCode::PluginInvalidOutput
        );
        let refused = br#"{"success":false,"error_message":"no","retryable":false}"#;
        let CallEnd::Failed(refusal) = end(Ok(refused.to_vec())) else {
            panic!("refused");
        };
        assert_eq!(refusal.class, SystemErrorClass::Stands);
        let ok = br#"{"success":true,"error_message":null}"#;
        assert_eq!(end(Ok(ok.to_vec())), CallEnd::Handled);
        let lost = DispatchFaults::default();
        lost.record(OperationFault::OperationLost, "lost");
        let run =
            CallEnd::of::<broccoli_server_sdk::types::OnCodeRunOutput>(Ok(ok.to_vec()), &lost);
        assert_eq!(
            code(run),
            HostCode::PluginError,
            "a lost attempt fails, to be handed back"
        );
    }

    /// A contest handler missing here hands the attempt back while an
    /// enabled plugin provides the contest type; with none, a unit taken
    /// without one loaded stands, naming the key.
    #[test]
    fn a_missing_handler_stands_only_when_no_enabled_plugin_provides_it() {
        let provided = crate::providers::Unresolved {
            kind: crate::providers::Provided::ContestType,
            key: "x".into(),
            provider: Some("contest".into()),
        };
        for needs_loaded in [true, false] {
            let lost = DispatchFailure::no_handler(needs_loaded, &provided);
            assert_eq!(
                (lost.code, lost.class),
                (HostCode::PluginNotLoadedHere, SystemErrorClass::HandBack)
            );
        }
        let unprovided = crate::providers::Unresolved {
            provider: None,
            ..provided
        };
        let missing = DispatchFailure::no_handler(false, &unprovided);
        assert_eq!(
            (missing.code, missing.class),
            (HostCode::NoProvider, SystemErrorClass::Stands)
        );
    }

    #[test]
    fn a_hand_back_fault_loses_the_attempt() {
        let faults = DispatchFaults::default();
        assert!(faults.lost().is_none());
        faults.record(OperationFault::NoCapableWorker, "none accepting");
        assert_eq!(
            faults.lost().map(|r| r.fault),
            Some(OperationFault::NoCapableWorker.into())
        );
    }

    #[test]
    fn the_first_fault_recorded_decides() {
        let faults = DispatchFaults::default();
        faults.record(OperationFault::OperationLost, "lost");
        faults.record(OperationFault::PlatformToolMissing, "no tool");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::OperationLost.into())
        );
    }

    #[test]
    fn a_lost_attempt_overrides_what_it_found() {
        let faults = DispatchFaults::default();
        faults.record(DispatchFault::CheckerFailed, "signal 6");
        assert!(faults.lost().is_none());
        faults.record(OperationFault::NoCapableWorker, "none accepting");
        faults.record(DispatchFault::CheckerFailed, "signal 6");
        assert_eq!(
            faults.lost().map(|r| r.fault),
            Some(OperationFault::NoCapableWorker.into())
        );
    }
}
