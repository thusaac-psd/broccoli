use std::collections::HashMap;

use serde::de::DeserializeOwned;

use crate::error::SdkError;
use crate::types::StorageSnapshot;

pub struct Storage {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: StorageMock,
}

#[cfg(target_arch = "wasm32")]
impl Storage {
    pub fn get(&self, keys: &[&str]) -> Result<HashMap<String, String>, SdkError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let input = serde_json::json!({ "keys": keys });
        let result_json = unsafe { crate::host::raw::store_get(serde_json::to_string(&input)?)? };
        let result: serde_json::Value = serde_json::from_str(&result_json)?;
        let values = result
            .get("values")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(values)
    }

    pub fn get_one(&self, key: &str) -> Result<Option<String>, SdkError> {
        let mut map = self.get(&[key])?;
        Ok(map.remove(key))
    }

    pub fn set(&self, entries: &[(&str, &str)]) -> Result<(), SdkError> {
        if entries.is_empty() {
            return Ok(());
        }
        let entries_json: Vec<serde_json::Value> = entries
            .iter()
            .map(|(k, v)| serde_json::json!({ "key": k, "value": v }))
            .collect();
        let input = serde_json::json!({ "entries": entries_json });
        unsafe { crate::host::raw::store_set(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn delete(&self, keys: &[&str]) -> Result<(), SdkError> {
        if keys.is_empty() {
            return Ok(());
        }
        let input = serde_json::json!({ "keys": keys });
        unsafe { crate::host::raw::store_delete(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&str>,
        new: &str,
    ) -> Result<bool, SdkError> {
        let input = serde_json::json!({
            "key": key,
            "expected": expected,
            "new": new,
        });
        let result_json =
            unsafe { crate::host::raw::store_compare_and_set(serde_json::to_string(&input)?)? };
        let result: serde_json::Value = serde_json::from_str(&result_json)?;
        Ok(result["swapped"].as_bool().unwrap_or(false))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct StorageMock {
    data: std::cell::RefCell<HashMap<String, String>>,
    /// Number of `Storage::get()` calls made so far. Note this mock's
    /// `get_one` reads `data` directly rather than calling `get` (unlike the
    /// real wasm32 impl, where `get_one` is `get(&[key])`), so it does NOT
    /// bump this counter - only `get()` itself does. That asymmetry is
    /// exactly the point: it lets a plugin-side test assert that a batch of
    /// lookups issued exactly one real `get()` (one extism host-fn crossing
    /// on the wasm32 side) rather than one `get_one()` per key, mirroring
    /// `host.db.queries()`'s role in the SQL-batching regression tests.
    get_calls: std::cell::RefCell<usize>,
    revisions: std::cell::RefCell<HashMap<String, i64>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageMock {
    pub fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(HashMap::new()),
            get_calls: std::cell::RefCell::new(0),
            revisions: std::cell::RefCell::new(HashMap::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage {
    pub fn get(&self, keys: &[&str]) -> Result<HashMap<String, String>, SdkError> {
        *self.inner.get_calls.borrow_mut() += 1;
        let data = self.inner.data.borrow();
        Ok(keys
            .iter()
            .filter_map(|k| data.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect())
    }

    /// Number of `get()` calls made so far (test-only instrumentation - see
    /// `StorageMock::get_calls`'s doc comment).
    pub fn get_call_count(&self) -> usize {
        *self.inner.get_calls.borrow()
    }

    pub fn get_one(&self, key: &str) -> Result<Option<String>, SdkError> {
        Ok(self.inner.data.borrow().get(key).cloned())
    }

    pub fn set(&self, entries: &[(&str, &str)]) -> Result<(), SdkError> {
        let mut data = self.inner.data.borrow_mut();
        for (k, v) in entries {
            data.insert(k.to_string(), v.to_string());
            *self
                .inner
                .revisions
                .borrow_mut()
                .entry(k.to_string())
                .or_default() += 1;
        }
        Ok(())
    }

    pub fn delete(&self, keys: &[&str]) -> Result<(), SdkError> {
        let mut data = self.inner.data.borrow_mut();
        for key in keys {
            data.remove(*key);
        }
        Ok(())
    }

    pub fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&str>,
        new: &str,
    ) -> Result<bool, SdkError> {
        let mut data = self.inner.data.borrow_mut();
        let current = data.get(key).map(|s| s.as_str());
        if current == expected {
            data.insert(key.to_string(), new.to_string());
            *self
                .inner
                .revisions
                .borrow_mut()
                .entry(key.to_string())
                .or_default() += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn data(&self) -> HashMap<String, String> {
        self.inner.data.borrow().clone()
    }
}

impl Storage {
    /// A storage revision changes on every write and atomic admission, even if
    /// the value stays identical. Plugins need not encode host counters in data.
    pub fn modify<T, F>(&self, key: &str, f: F) -> Result<T, SdkError>
    where
        T: serde::Serialize + DeserializeOwned + Default,
        F: Fn(&mut T) -> Result<(), SdkError>,
    {
        for _ in 0..100 {
            let snapshot = self.snapshot(key)?;
            let mut value: T = match &snapshot {
                Some(snapshot) => serde_json::from_str(&snapshot.value)?,
                None => T::default(),
            };
            f(&mut value)?;
            let new = serde_json::to_string(&value)?;
            if self.compare_and_set_snapshot(key, snapshot.as_ref(), &new)? {
                return Ok(value);
            }
        }
        Err(SdkError::Other("CAS retry limit exceeded".into()))
    }
}

#[cfg(target_arch = "wasm32")]
impl Storage {
    pub fn snapshot(&self, key: &str) -> Result<Option<StorageSnapshot>, SdkError> {
        let input = serde_json::json!({"keys":[key]});
        let json = unsafe { crate::host::raw::store_get(input.to_string())? };
        let response: serde_json::Value = serde_json::from_str(&json)?;
        let Some(value) = response["values"][key].as_str() else {
            return Ok(None);
        };
        let revision = response["versions"][key]
            .as_i64()
            .ok_or_else(|| SdkError::Other("Host did not return a storage revision".into()))?;
        Ok(Some(StorageSnapshot {
            value: value.to_owned(),
            revision,
        }))
    }
    pub fn compare_and_set_snapshot(
        &self,
        key: &str,
        expected: Option<&StorageSnapshot>,
        new: &str,
    ) -> Result<bool, SdkError> {
        let input = serde_json::json!({"key":key,"expected":expected.map(|s| &s.value),
            "expected_revision":expected.map(|s|s.revision),"new":new});
        let json = unsafe { crate::host::raw::store_compare_and_set(input.to_string())? };
        let response: serde_json::Value = serde_json::from_str(&json)?;
        Ok(response["swapped"].as_bool().unwrap_or(false))
    }
    /// Bounded, lexicographically ordered scan of the caller's storage namespace.
    pub fn scan(
        &self,
        prefix: &str,
        after: Option<&str>,
    ) -> Result<HashMap<String, String>, SdkError> {
        let input = serde_json::json!({"prefix":prefix,"after":after});
        let json = unsafe { crate::host::raw::store_scan(input.to_string())? };
        Ok(serde_json::from_str(&json)?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage {
    pub fn snapshot(&self, key: &str) -> Result<Option<StorageSnapshot>, SdkError> {
        Ok(self
            .inner
            .data
            .borrow()
            .get(key)
            .map(|value| StorageSnapshot {
                value: value.clone(),
                revision: self.inner.revisions.borrow().get(key).copied().unwrap_or(0),
            }))
    }
    pub fn compare_and_set_snapshot(
        &self,
        key: &str,
        expected: Option<&StorageSnapshot>,
        new: &str,
    ) -> Result<bool, SdkError> {
        let current = self.snapshot(key)?;
        if current.as_ref().map(|s| (&s.value, s.revision))
            != expected.map(|s| (&s.value, s.revision))
        {
            return Ok(false);
        }
        self.compare_and_set(key, expected.map(|s| s.value.as_str()), new)
    }
    pub fn scan(
        &self,
        prefix: &str,
        after: Option<&str>,
    ) -> Result<HashMap<String, String>, SdkError> {
        let mut keys: Vec<_> = self
            .inner
            .data
            .borrow()
            .keys()
            .filter(|key| key.starts_with(prefix) && after.is_none_or(|after| key.as_str() > after))
            .cloned()
            .collect();
        keys.sort();
        keys.truncate(100);
        self.get(&keys.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod revision_tests {
    #[test]
    fn identical_value_writes_invalidate_old_snapshots() {
        let host = crate::Host::mock();
        host.storage.set(&[("document", "opaque bytes")]).unwrap();
        let snapshot = host.storage.snapshot("document").unwrap().unwrap();
        host.storage.set(&[("document", "opaque bytes")]).unwrap();
        assert!(
            !host
                .storage
                .compare_and_set_snapshot("document", Some(&snapshot), "new")
                .unwrap()
        );
    }
}
