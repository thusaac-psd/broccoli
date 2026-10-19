use crate::error::SdkError;
use crate::types::PublishNotification;

#[derive(Default)]
pub struct Notifications {
    #[cfg(not(target_arch = "wasm32"))]
    records: std::cell::RefCell<Vec<PublishNotification>>,
    #[cfg(not(target_arch = "wasm32"))]
    fail_next: std::cell::Cell<bool>,
}

impl Notifications {
    /// Publish once per `(contest, recipient, key)`. Repeated calls preserve the
    /// first payload. Requires the manifest's `notifications` host permission.
    pub fn publish(&self, message: &PublishNotification) -> Result<(), SdkError> {
        message.validate().map_err(SdkError::Other)?;
        #[cfg(target_arch = "wasm32")]
        unsafe {
            crate::host::raw::notification_publish(serde_json::to_string(message)?)?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.fail_next.replace(false) {
                return Err(SdkError::Other("mock notification failure".into()));
            }
            let mut records = self.records.borrow_mut();
            let mut new = message.clone();
            new.user_ids.sort_unstable();
            new.user_ids.dedup();
            new.user_ids.retain(|id| {
                !records.iter().any(|r| {
                    r.contest_id == message.contest_id
                        && r.key == message.key
                        && r.user_ids.contains(id)
                })
            });
            if !new.user_ids.is_empty() {
                records.push(new);
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn published(&self) -> Vec<PublishNotification> {
        self.records.borrow().clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn fail_next_publish(&self) {
        self.fail_next.set(true);
    }
}
