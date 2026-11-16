use std::sync::Arc;

use async_trait::async_trait;
use opentelemetry::KeyValue;

use super::error::StorageError;
use super::hash::ContentHash;
use super::traits::{BlobStore, BoxReader};

pub struct InstrumentedBlobStore {
    inner: Arc<dyn BlobStore>,
    backend: String,
    metrics: crate::metrics::Metrics,
}

impl InstrumentedBlobStore {
    pub fn new(
        inner: Arc<dyn BlobStore>,
        backend: impl Into<String>,
        metrics: crate::metrics::Metrics,
    ) -> Self {
        Self {
            inner,
            backend: backend.into(),
            metrics,
        }
    }

    fn record(
        &self,
        operation: &'static str,
        outcome: &'static str,
        start: std::time::Instant,
        bytes: Option<u64>,
    ) {
        let attrs = [
            KeyValue::new("storage.backend", self.backend.clone()),
            KeyValue::new("operation", operation),
            KeyValue::new("outcome", outcome),
        ];
        self.metrics
            .blob_store_operation_duration
            .record(start.elapsed().as_secs_f64(), &attrs);
        self.metrics.blob_store_operations_total.add(1, &attrs);
        if outcome == "error" {
            self.metrics.blob_store_errors_total.add(1, &attrs);
        }
        if let Some(bytes) = bytes {
            self.metrics.blob_store_bytes_total.add(bytes, &attrs);
        }
    }
}

#[async_trait]
impl BlobStore for InstrumentedBlobStore {
    async fn put(&self, data: &[u8]) -> Result<ContentHash, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.put(data).await;
        self.record(
            "put",
            if result.is_ok() { "success" } else { "error" },
            start,
            Some(data.len() as u64),
        );
        result
    }

    async fn put_stream(&self, reader: BoxReader) -> Result<ContentHash, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.put_stream(reader).await;
        self.record(
            "put_stream",
            if result.is_ok() { "success" } else { "error" },
            start,
            None,
        );
        result
    }

    async fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.get(hash).await;
        self.record(
            "get",
            if result.is_ok() { "success" } else { "error" },
            start,
            result.as_ref().ok().map(|bytes| bytes.len() as u64),
        );
        result
    }

    async fn get_stream(&self, hash: &ContentHash) -> Result<BoxReader, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.get_stream(hash).await;
        self.record(
            "get_stream",
            if result.is_ok() { "success" } else { "error" },
            start,
            None,
        );
        result
    }

    async fn get_range(
        &self,
        hash: &ContentHash,
        offset: u64,
        len: usize,
    ) -> Result<(Vec<u8>, bool), StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.get_range(hash, offset, len).await;
        self.record(
            "get_range",
            if result.is_ok() { "success" } else { "error" },
            start,
            result.as_ref().ok().map(|(bytes, _)| bytes.len() as u64),
        );
        result
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.exists(hash).await;
        self.record(
            "exists",
            if result.is_ok() { "success" } else { "error" },
            start,
            None,
        );
        result
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.delete(hash).await;
        self.record(
            "delete",
            if result.is_ok() { "success" } else { "error" },
            start,
            None,
        );
        result
    }

    async fn size(&self, hash: &ContentHash) -> Result<u64, StorageError> {
        let start = std::time::Instant::now();
        let result = self.inner.size(hash).await;
        self.record(
            "size",
            if result.is_ok() { "success" } else { "error" },
            start,
            result.as_ref().ok().copied(),
        );
        result
    }

    async fn probe(&self) -> Result<(), StorageError> {
        self.inner.probe().await
    }
}
