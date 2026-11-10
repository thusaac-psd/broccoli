//! Operation execution on the mock sandbox. The shared cases in
//! `sandbox_cases` run here on every `cargo test`; the isolate suite runs the
//! same cases against the real sandbox.

mod sandbox_cases;

use common::storage::BlobStore;
use common::storage::object_storage::{ObjectStorageBlobStore, ObjectStorageConfig};
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

    fn run_options() -> RunOptions {
        RunOptions::default()
    }
}

sandbox_suite!(Mock);

fn object_storage_env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

async fn try_object_storage_store_for_test() -> Option<Arc<dyn BlobStore>> {
    let endpoint = object_storage_env("BROCCOLI_S3_ENDPOINT", "http://127.0.0.1:8333");
    let bucket = object_storage_env("BROCCOLI_S3_BUCKET", "broccoli-blobs");
    let region = object_storage_env("BROCCOLI_S3_REGION", "us-east-1");
    let access_key = object_storage_env("BROCCOLI_S3_ACCESS_KEY", "broccoli_s3_access");
    let secret_key = object_storage_env("BROCCOLI_S3_SECRET_KEY", "broccoli_s3_secret");
    let path_style = object_storage_env("BROCCOLI_S3_PATH_STYLE", "true")
        .trim()
        .eq_ignore_ascii_case("true");

    let store = match ObjectStorageBlobStore::new(
        ObjectStorageConfig {
            bucket,
            region,
            endpoint: Some(endpoint),
            access_key: Some(access_key),
            secret_key: Some(secret_key),
            path_style,
            max_size: 128 * 1024 * 1024,
            temp_dir: None,
        },
        None,
    ) {
        Ok(store) => Arc::new(store) as Arc<dyn BlobStore>,
        Err(err) => {
            eprintln!("skip object storage test: invalid config: {err}");
            return None;
        }
    };

    match store.put(b"worker-object-storage-probe").await {
        Ok(hash) => {
            let _ = store.delete(&hash).await;
            Some(store)
        }
        Err(err) => {
            eprintln!("skip object storage test: backend unavailable: {err}");
            None
        }
    }
}

#[tokio::test]
#[ignore = "requires configured object storage reachable through WORKER_OBJECT_STORAGE_* env vars"]
async fn execute_operation_with_file_pulled_from_object_storage() {
    let store = try_object_storage_store_for_test()
        .await
        .expect("object storage worker test backend unavailable");

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
        priority: None,
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
