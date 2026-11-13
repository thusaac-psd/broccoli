use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("Config error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("MQ error: {0}")]
    Mq(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("External error: {0}")]
    External(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Task panicked: {0}")]
    TaskPanic(String),
}
