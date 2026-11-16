//! Operation execution on the mock sandbox. The shared cases in
//! `sandbox_cases` run here on every `cargo test`; the isolate suite runs the
//! same cases against the real sandbox.

mod sandbox_cases;

use common::storage::BlobStore;
use common::storage::object_storage::testing::Seaweedfs;
use sandbox_cases::{Backend, unique_temp_dir};
use std::sync::Arc;
use worker::models::operation::file_cacher::BlobStoreFileCacher;
use worker::models::operation::handler::OperationHandler;
use worker::models::operation::models::{
    Environment, IOConfig, OperationTask, SessionFile, Step, StepKind,
};
use worker::models::operation::sandbox::mock::MockSandboxManager;
use worker::models::operation::sandbox::{RunOptions, SandboxManager};
use worker::models::operation::task_cache::NoopTaskCacheStore;

struct Mock;

impl Backend for Mock {
    fn manager() -> Box<dyn SandboxManager + Send + Sync> {
        Box::new(MockSandboxManager::new(unique_temp_dir(
            "mock-sandbox-test",
        )))
    }

    // The mock runs each step with its box as the working directory and
    // materializes mounts inside it, so a mounted path is box-relative.
    fn mounted(path: &str) -> String {
        format!("./{path}")
    }
}

sandbox_suite!(
    Mock;
    all: [];
);

#[tokio::test]
async fn execute_operation_with_file_pulled_from_object_storage() {
    let seaweedfs = Seaweedfs::start().await;
    seaweedfs.create_bucket("broccoli-blobs").await;
    let store: Arc<dyn BlobStore> = Arc::new(seaweedfs.store("broccoli-blobs"));

    let hash = store
        .put(b"40 2\n")
        .await
        .expect("put test object should succeed");
    let hash_hex = hash.to_hex();

    let cache_root = tempfile::tempdir().expect("create temp cache dir");
    let cacher = BlobStoreFileCacher::new(
        store,
        cache_root.path().join("cache"),
        64 * 1024 * 1024,
        None,
    )
    .await
    .expect("create blob store file cacher should succeed");

    let (metrics, _registry) = common::observability::init_metrics("broccoli-worker-test");
    let handler = OperationHandler::new(
        Box::new(MockSandboxManager::new(unique_temp_dir(
            "mock-sandbox-test",
        ))),
        Box::new(cacher),
        Box::new(NoopTaskCacheStore),
        String::new(),
        metrics,
    );

    let operation = OperationTask {
        environments: vec![Environment {
            id: "env-1".to_string(),
            files_in: vec![(
                "input.txt".to_string(),
                SessionFile::Blob { hash: hash_hex },
            )],
        }],
        tasks: vec![
            Step {
                id: "run".to_string(),
                kind: StepKind::Generic,
                env_ref: "env-1".to_string(),
                argv: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "awk '{print $1 + $2}' input.txt > output.txt".to_string(),
                ],
                conf: RunOptions::default(),
                io: IOConfig::default(),
                collect: vec![],
                depends_on: vec![],
                cache: None,
                mounts: vec![],
            },
            Step {
                id: "verify".to_string(),
                kind: StepKind::Generic,
                env_ref: "env-1".to_string(),
                argv: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "grep -qx '42' output.txt && echo pulled-ok".to_string(),
                ],
                conf: RunOptions::default(),
                io: IOConfig::default(),
                collect: vec![],
                depends_on: vec!["run".to_string()],
                cache: None,
                mounts: vec![],
            },
        ],
        channels: vec![],
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    };

    let result = handler
        .execute(&operation)
        .await
        .expect("operation execution should succeed");
    assert!(result.success, "operation should succeed: {result:?}");

    let verify = result
        .task_results
        .get("verify")
        .expect("verify step should exist");
    assert!(verify.success, "verify step should succeed: {verify:?}");
    assert!(
        verify.sandbox_result.stdout.contains("pulled-ok"),
        "verify output should indicate object file was pulled"
    );
    eprintln!("successfully fetch file from object storage and use in mock sandbox");
}
