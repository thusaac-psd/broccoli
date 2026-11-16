use std::io::Cursor;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::error::StorageError;
use super::hash::ContentHash;

pub type BoxReader = Box<dyn AsyncRead + Unpin + Send>;

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, data: &[u8]) -> Result<ContentHash, StorageError> {
        let reader: BoxReader = Box::new(Cursor::new(data.to_vec()));
        self.put_stream(reader).await
    }

    async fn put_stream(&self, reader: BoxReader) -> Result<ContentHash, StorageError>;

    async fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StorageError> {
        let mut reader = self.get_stream(hash).await?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn get_stream(&self, hash: &ContentHash) -> Result<BoxReader, StorageError>;

    async fn get_range(
        &self,
        hash: &ContentHash,
        offset: u64,
        len: usize,
    ) -> Result<(Vec<u8>, bool), StorageError> {
        let mut reader = self.get_stream(hash).await?;
        let mut remaining_skip = offset;
        let mut discard = [0u8; 64 * 1024];
        while remaining_skip > 0 {
            let want = remaining_skip.min(discard.len() as u64) as usize;
            let n = reader.read(&mut discard[..want]).await?;
            if n == 0 {
                return Ok((Vec::new(), true));
            }
            remaining_skip -= n as u64;
        }

        let mut bytes = vec![0u8; len];
        let mut filled = 0usize;
        while filled < len {
            let n = reader.read(&mut bytes[filled..]).await?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        bytes.truncate(filled);
        Ok((bytes, filled < len))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool, StorageError>;

    async fn delete(&self, hash: &ContentHash) -> Result<bool, StorageError>;

    async fn size(&self, hash: &ContentHash) -> Result<u64, StorageError>;

    /// Whether the store can serve requests now, for health checks: cheap, and
    /// answered from a recent check where reaching the store costs a round
    /// trip. A store inside a dependency the caller already probes (the
    /// database) answers `Ok`.
    async fn probe(&self) -> Result<(), StorageError> {
        Ok(())
    }
}
