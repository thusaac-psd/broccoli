//! Platform faults under a dispatch, and what the system-error retry does
//! with the judgement they end.
//!
//! Everything that works for one dispatch (its plugin calls, the operations
//! they publish, the evaluate batches and detached sessions they start)
//! shares one [`DispatchFaults`]. Whatever observes a platform fault records
//! it there: a publish the queue refused, an operation the queue lost, a
//! worker without a platform tool. When the dispatch then ends its judgement
//! as a system error, the judgement carries the fault's code
//! ([`OperationFault::code`]) instead of whatever the plugin reported, and
//! [`SystemErrorClass::of`] decides what the retry does with it. A plugin
//! never classifies a failure itself.

use std::sync::{Arc, Mutex};

use common::SubmissionDlqErrorCode;
use common::worker::OperationFault;
use plugin_core::executor::CallLocals;

/// The platform fault that decides a dispatch's system error, if any.
#[derive(Clone, Default, Debug)]
pub struct DispatchFaults(Arc<Mutex<Option<Recorded>>>);

/// A recorded fault with the operator-facing description of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub fault: OperationFault,
    pub message: String,
}

/// The [`DispatchFaults`] a call tree's [`CallLocals`] hold.
#[derive(Default)]
struct CallFaults(DispatchFaults);

impl DispatchFaults {
    /// Record `fault`; the first one recorded stays.
    pub fn record(&self, fault: OperationFault, message: impl Into<String>) {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
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
    /// [`SystemErrorClass::HandBack`]. A lost attempt never becomes a
    /// verdict: its judging session ends without asking the plugin, a
    /// plugin write that would finalize it is fenced out, and the dispatch
    /// hands the judgement back (`fail_dispatch`).
    pub fn lost(&self) -> Option<Recorded> {
        self.get()
            .filter(|r| SystemErrorClass::of_fault(r.fault) == SystemErrorClass::HandBack)
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
    /// The contest plugin refused the submission: it stands at once.
    Refused,
}

impl SystemErrorClass {
    pub(crate) fn of_fault(fault: OperationFault) -> Self {
        match fault {
            OperationFault::QueueUnreachable
            | OperationFault::OperationLost
            | OperationFault::PlatformToolMissing
            | OperationFault::NoCapableWorker => Self::HandBack,
        }
    }

    /// The class of a system error carrying `code`.
    pub(crate) fn of_code(code: &str) -> Self {
        [Self::HandBack, Self::Refused]
            .into_iter()
            .find(|class| class.codes().contains(&code))
            .unwrap_or(Self::Judging)
    }

    /// The error codes of this class; a `Judging` error carries any other
    /// code, or none.
    fn codes(self) -> Vec<&'static str> {
        match self {
            Self::Judging => Vec::new(),
            Self::Refused => vec![SubmissionDlqErrorCode::PLUGIN_REFUSED],
            Self::HandBack => OperationFault::ALL
                .iter()
                .copied()
                .filter(|f| Self::of_fault(*f) == self)
                .map(OperationFault::code)
                .collect(),
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
                let special = [Self::HandBack, Self::Refused]
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
        for fault in OperationFault::ALL {
            let class = SystemErrorClass::of_fault(fault);
            assert!(class.codes().contains(&fault.code()), "{fault:?}");
            assert_eq!(SystemErrorClass::of_code(fault.code()), class, "{fault:?}");
            assert!(!SystemErrorClass::Judging.sql_matches("c").contains("IN ()"));
        }
        assert_eq!(
            SystemErrorClass::of_code(SubmissionDlqErrorCode::PLUGIN_REFUSED),
            SystemErrorClass::Refused
        );
        assert_eq!(
            SystemErrorClass::of_code("PLUGIN_NO_VERDICT"),
            SystemErrorClass::Judging
        );
    }

    #[test]
    fn a_hand_back_fault_loses_the_attempt() {
        let faults = DispatchFaults::default();
        assert!(faults.lost().is_none());
        faults.record(OperationFault::NoCapableWorker, "none accepting");
        assert_eq!(
            faults.lost().map(|r| r.fault),
            Some(OperationFault::NoCapableWorker)
        );
    }

    #[test]
    fn the_first_fault_recorded_decides() {
        let faults = DispatchFaults::default();
        faults.record(OperationFault::OperationLost, "lost");
        faults.record(OperationFault::PlatformToolMissing, "no tool");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::OperationLost)
        );
    }

    #[test]
    fn a_seeded_call_shares_the_dispatchs_faults() {
        let faults = DispatchFaults::default();
        let locals = faults.call_locals();
        DispatchFaults::of(&locals).record(OperationFault::OperationLost, "lost");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::OperationLost)
        );
    }
}
