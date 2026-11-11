//! Shared fixtures for the crate's unit tests.

use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

/// A throwaway Postgres 17 with the full schema (`database::init_db`), for
/// unit tests that need real SQL semantics `MockDatabase` cannot give. Keep
/// the container alive for the test's duration.
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

/// A user and a problem to hang test submissions on. Returns their ids.
pub(crate) async fn seed_user_and_problem(db: &DatabaseConnection) -> (i32, i32) {
    use crate::entity::{problem, user};
    let now = Utc::now();
    let u = user::ActiveModel {
        username: Set("test-user".into()),
        display_name: Set("test-user".into()),
        password: Set("x".into()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert user");
    let p = problem::ActiveModel {
        title: Set("test-problem".into()),
        content: Set("c".into()),
        time_limit: Set(1000),
        memory_limit: Set(262_144),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert problem");
    (u.id, p.id)
}
