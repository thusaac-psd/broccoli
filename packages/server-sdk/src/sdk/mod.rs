mod checker;
mod code_runs;
mod config;
mod db;
mod eval;
mod language;
mod logger;
mod operations;
mod registry;
mod shared;
mod storage;
mod submissions;
mod timer;

pub use checker::Checker;
pub use code_runs::CodeRuns;
pub use config::Config;
pub use db::{Db, Transaction};
pub use eval::Eval;
pub use language::Language;
pub use logger::Logger;
pub use operations::Operations;
pub use registry::Registry;
pub use storage::Storage;
pub use submissions::Submissions;
pub use timer::Timer;

#[cfg(not(target_arch = "wasm32"))]
pub use db::{RecordedExecution, RecordedQuery};

pub struct Host {
    pub db: Db,
    pub eval: Eval,
    pub submission: Submissions,
    pub code_run: CodeRuns,
    pub log: Logger,
    pub storage: Storage,
    pub config: Config,
    pub operations: Operations,
    pub checker: Checker,
    pub language: Language,
    pub registry: Registry,
    pub timer: Timer,
}

#[cfg(target_arch = "wasm32")]
impl Host {
    pub fn new() -> Self {
        Self {
            db: Db {},
            eval: Eval {},
            submission: Submissions {},
            code_run: CodeRuns {},
            log: Logger {},
            storage: Storage {},
            config: Config {},
            operations: Operations {},
            checker: Checker {},
            language: Language {},
            registry: Registry {},
            timer: Timer {},
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Host {
    pub fn mock() -> Self {
        Self {
            db: Db {
                inner: db::DbMock::new(),
            },
            eval: Eval {
                inner: eval::EvalMock::new(),
            },
            submission: Submissions {
                inner: submissions::SubmissionsMock::new(),
            },
            code_run: CodeRuns {
                inner: code_runs::CodeRunsMock::new(),
            },
            log: Logger {
                inner: logger::LoggerMock::new(),
            },
            storage: Storage {
                inner: storage::StorageMock::new(),
            },
            config: Config {
                inner: config::ConfigMock::new(),
            },
            operations: Operations {
                inner: operations::OperationsMock::new(),
            },
            checker: Checker {},
            language: Language {},
            registry: Registry {},
            timer: Timer {},
        }
    }
}
