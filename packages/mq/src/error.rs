/// A queue operation that did not happen.
#[derive(Debug, thiserror::Error)]
pub enum MqError {
    #[error("redis: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("payload does not serialize: {0}")]
    Encode(#[from] serde_json::Error),
}
