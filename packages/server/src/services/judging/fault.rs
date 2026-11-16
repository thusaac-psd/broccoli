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

use common::SubmissionDlqErrorCode;
use common::worker::OperationFault;
use plugin_core::executor::CallLocals;

/// A fault one dispatch met that decides its system error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchFault {
    /// The platform lost one of its operations.
    Operation(OperationFault),
    /// The problem's checker program failed (crashed, was killed by a
    /// signal, exited with a code its format gives no meaning), as its
    /// checker plugin reported through `CheckerVerdict::checker_failure`.
    CheckerFailed,
}

impl DispatchFault {
    /// The error code a judgement this fault ends carries.
    pub fn code(self) -> &'static str {
        match self {
            Self::Operation(fault) => fault.code(),
            Self::CheckerFailed => "CHECKER_FAILED",
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

/// The [`DispatchFaults`] a call tree's [`CallLocals`] hold.
#[derive(Default)]
struct CallFaults(DispatchFaults);

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

    /// The faults of the plugin call tree running on this thread, if any.
    pub fn of_current_call() -> Option<Self> {
        plugin_core::executor::call_locals().map(|locals| Self::of(&locals))
    }

    fn of(locals: &CallLocals) -> Self {
        locals.with(|f: &mut CallFaults| f.0.clone())
    }

    /// Fresh call locals for a plugin call made for this dispatch: the call's
    /// host functions record into, and read, these faults.
    pub fn call_locals(&self) -> Arc<CallLocals> {
        let locals: Arc<CallLocals> = Arc::default();
        locals.with(|f: &mut CallFaults| f.0 = self.clone());
        locals
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
    /// operation, the worker that took it lacks a platform tool, or no worker
    /// could take it): handed back to the queue at once, spending no budget,
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
            ) => Self::HandBack,
            DispatchFault::CheckerFailed => Self::Stands,
        }
    }
}

/// Why a dispatch ended its unit without a verdict: the code and message the
/// unit records, and the class that decides what becomes of it
/// ([`super::fail_dispatch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DispatchFailure {
    pub code: &'static str,
    pub message: String,
    pub class: SystemErrorClass,
}

impl DispatchFailure {
    /// A failure judging the submission, retried within its budget.
    pub(crate) fn judging(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            class: SystemErrorClass::Judging,
        }
    }

    /// The contest plugin refused the work as one every attempt would fail.
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self {
            code: SubmissionDlqErrorCode::PLUGIN_REFUSED,
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
                DispatchFailure::judging("PLUGIN_ERROR", message)
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
                "PLUGIN_ERROR",
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
            Err(e) if e.is_unavailable() => {
                return Self::Failed(DispatchFailure::judging(
                    "PLUGIN_UNAVAILABLE",
                    e.to_string(),
                ));
            }
            Err(e) => {
                return Self::Failed(DispatchFailure::judging(
                    "PLUGIN_EXECUTION_ERROR",
                    e.to_string(),
                ));
            }
        };
        let output = match serde_json::from_slice::<O>(&bytes) {
            Ok(output) => output,
            Err(e) => {
                return Self::Failed(DispatchFailure::judging(
                    "PLUGIN_INVALID_OUTPUT",
                    format!("Plugin returned invalid output: {e}"),
                ));
            }
        };
        match output.failure() {
            Some(failure) => Self::Failed(failure),
            // The plugin's verdict, if it wrote one, was fenced out.
            None if faults.lost().is_some() => Self::Failed(DispatchFailure::judging(
                "PLUGIN_ERROR",
                "The platform lost the attempt",
            )),
            None => Self::Handled,
        }
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
        let own = DispatchFailure::judging("PLUGIN_ERROR", "boom");
        assert_eq!(own.clone().unless_fault(&faults), own);
        faults.record(DispatchFault::CheckerFailed, "signal 6");
        let failure = own.unless_fault(&faults);
        assert_eq!(failure.code, "CHECKER_FAILED");
        assert_eq!(failure.message, "signal 6");
        assert_eq!(failure.class, SystemErrorClass::Stands);
    }

    fn end(result: Result<Vec<u8>, plugin_core::error::PluginError>) -> CallEnd {
        CallEnd::of::<broccoli_server_sdk::types::OnSubmissionOutput>(
            result,
            &DispatchFaults::default(),
        )
    }

    fn code(end: CallEnd) -> &'static str {
        match end {
            CallEnd::Failed(failure) => failure.code,
            other => panic!("{other:?}"),
        }
    }

    /// Every way a plugin call fails maps to one outcome, the same for a
    /// judgement and a code run: a call the host could not serve is
    /// unavailable, whatever made it so.
    #[test]
    fn a_plugin_call_error_maps_to_one_dispatch_outcome() {
        use plugin_core::error::PluginError;
        let unavailable = [
            PluginError::Unavailable {
                plugin_id: "p".into(),
                func_name: "f".into(),
            },
            PluginError::NotLoaded("p".into()),
            PluginError::InstanceBuild {
                plugin_id: "p".into(),
                message: "oom".into(),
            },
        ];
        for e in unavailable {
            assert_eq!(code(end(Err(e))), "PLUGIN_UNAVAILABLE");
        }
        let failed = PluginError::ExecutionFailed {
            plugin_id: "p".into(),
            func_name: "f".into(),
            depth: 1,
            message: "trap".into(),
        };
        assert_eq!(code(end(Err(failed))), "PLUGIN_EXECUTION_ERROR");
        assert_eq!(end(Err(PluginError::ShuttingDown)), CallEnd::ShuttingDown);
        assert_eq!(code(end(Ok(b"nope".to_vec()))), "PLUGIN_INVALID_OUTPUT");
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
            "PLUGIN_ERROR",
            "a lost attempt fails, to be handed back"
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

    #[test]
    fn a_seeded_call_shares_the_dispatchs_faults() {
        let faults = DispatchFaults::default();
        let locals = faults.call_locals();
        DispatchFaults::of(&locals).record(OperationFault::OperationLost, "lost");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::OperationLost.into())
        );
    }
}
