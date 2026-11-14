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
    /// Record `fault`. A fault that would recur on every attempt replaces a
    /// hand-back, since retrying cannot help; otherwise the first stays.
    pub fn record(&self, fault: OperationFault, message: impl Into<String>) {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let replaces = match slot.as_ref() {
            None => true,
            Some(current) => {
                SystemErrorClass::of_fault(current.fault) == SystemErrorClass::HandBack
                    && SystemErrorClass::of_fault(fault) == SystemErrorClass::Deterministic
            }
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
    /// operation): requeued at once, spending no budget.
    HandBack,
    /// A platform misconfiguration that fails every attempt alike: the error
    /// stands at once and is dead-lettered for the operator.
    Deterministic,
    /// The contest plugin refused the submission: it stands at once.
    Refused,
}

impl SystemErrorClass {
    pub(crate) fn of_fault(fault: OperationFault) -> Self {
        match fault {
            OperationFault::QueueUnreachable | OperationFault::OperationLost => Self::HandBack,
            OperationFault::PlatformToolMissing => Self::Deterministic,
        }
    }

    /// The error codes of this class; a `Judging` error carries any other
    /// code, or none.
    fn codes(self) -> Vec<&'static str> {
        match self {
            Self::Judging => Vec::new(),
            Self::Refused => vec![SubmissionDlqErrorCode::PLUGIN_REFUSED],
            Self::HandBack | Self::Deterministic => OperationFault::ALL
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
                let special = [Self::HandBack, Self::Deterministic, Self::Refused]
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
            assert!(!SystemErrorClass::Judging.sql_matches("c").contains("IN ()"));
        }
    }

    #[test]
    fn a_deterministic_fault_outranks_a_hand_back() {
        let faults = DispatchFaults::default();
        faults.record(OperationFault::OperationLost, "lost");
        faults.record(OperationFault::PlatformToolMissing, "no tool");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::PlatformToolMissing)
        );
        faults.record(OperationFault::QueueUnreachable, "down");
        assert_eq!(
            faults.get().map(|r| r.fault),
            Some(OperationFault::PlatformToolMissing)
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
