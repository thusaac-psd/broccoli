//! Shared fixtures for the crate's unit tests.

use sea_orm::DatabaseConnection;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

/// A throwaway Postgres 17 with the full schema (`database::init_db`), for
/// unit tests that need real SQL semantics `MockDatabase` cannot give. Keep
/// the container alive for the test's duration.
///
/// The private `start_pg` helpers in `dispatcher/steal.rs`,
/// `dispatcher/system_error_retry.rs` and `services/submission_dispatch.rs`
/// are older copies of this one and can move onto it.
pub(crate) async fn postgres() -> (ContainerAsync<Postgres>, DatabaseConnection) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres host port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = crate::database::init_db(&url)
        .await
        .expect("init schema on test db");
    (container, db)
}
