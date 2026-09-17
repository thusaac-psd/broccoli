use std::collections::HashMap;

use serde::de::DeserializeOwned;

use crate::error::SdkError;

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

    pub fn modify<T, F>(&self, key: &str, f: F) -> Result<T, SdkError>
    where
        T: serde::Serialize + DeserializeOwned + Default,
        F: Fn(&mut T) -> Result<(), SdkError>,
    {
        for _ in 0..100 {
            let old_raw = self.get_one(key)?;
            let mut val: T = match &old_raw {
                Some(json) => serde_json::from_str(json)?,
                None => T::default(),
            };
            f(&mut val)?;
            let new_raw = serde_json::to_string(&val)?;
            if self.compare_and_set(key, old_raw.as_deref(), &new_raw)? {
                return Ok(val);
            }
        }
        Err(SdkError::Other("CAS retry limit exceeded".into()))
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
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageMock {
    pub fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(HashMap::new()),
            get_calls: std::cell::RefCell::new(0),
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
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn modify<T, F>(&self, key: &str, f: F) -> Result<T, SdkError>
    where
        T: serde::Serialize + DeserializeOwned + Default,
        F: Fn(&mut T) -> Result<(), SdkError>,
    {
        loop {
            let old_raw = self.get_one(key)?;
            let mut val: T = match &old_raw {
                Some(json) => serde_json::from_str(json)?,
                None => T::default(),
            };
            f(&mut val)?;
            let new_raw = serde_json::to_string(&val)?;
            if self.compare_and_set(key, old_raw.as_deref(), &new_raw)? {
                return Ok(val);
            }
        }
    }

    pub fn data(&self) -> HashMap<String, String> {
        self.inner.data.borrow().clone()
    }
}
