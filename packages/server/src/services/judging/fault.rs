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
//! plugin reported, and [`SystemErrorClass::of_fault`] decides what the
//! retry does with it. A plugin never classifies a failure itself.

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
    /// Every fault, beside the definition so a new one is listed with it.
    pub const ALL: [DispatchFault; 5] = [
        Self::Operation(OperationFault::ALL[0]),
        Self::Operation(OperationFault::ALL[1]),
        Self::Operation(OperationFault::ALL[2]),
        Self::Operation(OperationFault::ALL[3]),
        Self::CheckerFailed,
    ];

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

/// What the system-error retry does with a judgement that ended as a system
/// error, by the error code it carries.
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
    /// submission, or the problem's checker fails. It stands at once.
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

    /// The class of a system error carrying `code`.
    pub(crate) fn of_code(code: &str) -> Self {
        [Self::HandBack, Self::Stands]
            .into_iter()
            .find(|class| class.codes().contains(&code))
            .unwrap_or(Self::Judging)
    }

    /// The error codes of this class; a `Judging` error carries any other
    /// code, or none.
    fn codes(self) -> Vec<&'static str> {
        let faults = DispatchFault::ALL
            .into_iter()
            .filter(|f| Self::of_fault(*f) == self)
            .map(DispatchFault::code);
        match self {
            Self::Judging => Vec::new(),
            Self::Stands => [SubmissionDlqErrorCode::PLUGIN_REFUSED]
                .into_iter()
                .chain(faults)
                .collect(),
            Self::HandBack => faults.collect(),
        }
    }

    /// SQL: the error code `column` puts its row in this class.
    pub(crate) fn sql_matches(self, column: &str) -> String {
        let list = |codes: Vec<&str>| {
            let quoted: Vec<String> = codes.iter().map(|c| format!("'{c}'")).collect();
            format!("({})", quoted.join(", "))
        };
        match self {
            Self::Judging => {
                let special = [Self::HandBack, Self::Stands]
                    .into_iter()
                    .flat_map(Self::codes)
                    .collect();
                format!("({column} IS NULL OR {column} NOT IN {})", list(special))
            }
            other => format!(
                "({column} IS NOT NULL AND {column} IN {})",
                list(other.codes())
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fault_belongs_to_a_class_with_its_code() {
        for fault in DispatchFault::ALL {
            let class = SystemErrorClass::of_fault(fault);
            assert!(class.codes().contains(&fault.code()), "{fault:?}");
            assert_eq!(SystemErrorClass::of_code(fault.code()), class, "{fault:?}");
            assert!(!SystemErrorClass::Judging.sql_matches("c").contains("IN ()"));
        }
        assert_eq!(
            SystemErrorClass::of_code(SubmissionDlqErrorCode::PLUGIN_REFUSED),
            SystemErrorClass::Stands
        );
        for fault in OperationFault::ALL {
            assert!(DispatchFault::ALL.contains(&fault.into()), "{fault:?}");
        }
        assert_eq!(
            SystemErrorClass::of_code("PLUGIN_NO_VERDICT"),
            SystemErrorClass::Judging
        );
    }

    /// Only the host's own codes class as a hand-back or as standing: every
    /// one is bare, and a plugin's code is always recorded under its id
    /// (`plugin_writes::plugin_error_code`), so no code a plugin writes,
    /// whatever its text, can class as anything but a judging failure.
    #[test]
    fn no_code_a_plugin_writes_classes_as_a_host_code() {
        for class in [SystemErrorClass::HandBack, SystemErrorClass::Stands] {
            for code in class.codes() {
                assert!(!code.contains('.'), "{code} must stay bare");
                let as_plugin = super::super::plugin_writes::plugin_error_code("p", code);
                assert_eq!(
                    SystemErrorClass::of_code(&as_plugin),
                    SystemErrorClass::Judging,
                    "{as_plugin}"
                );
            }
        }
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
