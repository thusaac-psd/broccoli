use crate::MIN_REDIS_MAJOR;

/// A queue operation that did not happen.
#[derive(Debug, thiserror::Error)]
pub enum MqError {
    #[error("redis: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("payload does not serialize: {0}")]
    Encode(#[from] serde_json::Error),
    #[error(
        "Redis {found} is too old: the message queue needs Redis {MIN_REDIS_MAJOR} or newer \
         (it uses stream commands added in 6.2 and 7.0)"
    )]
    RedisTooOld { found: String },
    #[error(
        "Redis key {key} is a {found}, not a {expected}: it is not the message queue's. \
         Remove it or pick another mq.operation_queue_name"
    )]
    WrongKeyType {
        key: String,
        expected: &'static str,
        found: String,
    },
}
