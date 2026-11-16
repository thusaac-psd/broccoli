//! The migrated schema is the schema the code expects.
//!
//! The code describes what it expects in three places: the entities (tables,
//! columns, keys, relations), the trigger definitions the claim listener and
//! the plugin config cache rely on, and the predicates the queue scans use
//! inline so the planner can match their partial indexes. These tests migrate
//! an empty database and fail on any difference, so a schema change that
//! ships without its migration (or a migration without its code change) never
//! passes.

use std::collections::BTreeSet;

use common::StatusPhase;
use common::db::{DatabaseConnection, PgPoolSpec, PgSession};
use sea_orm::{ConnectionTrait, DbBackend, Schema, Statement};
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

use crate::database::BootSession;

async fn postgres() -> (ContainerAsync<Postgres>, u16) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    (container, port)
}

fn url(port: u16, database: &str) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{database}")
}

async fn connect(url: &str) -> DatabaseConnection {
    common::db::connect(
        url,
        &PgPoolSpec::single_session(),
        &PgSession::unbounded("schema-test"),
    )
    .await
    .expect("connect")
}

async fn rows(db: &DatabaseConnection, sql: &str) -> BTreeSet<String> {
    db.query_all_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .into_iter()
        .map(|row| row.try_get::<String>("", "fact").expect("fact"))
        .collect()
}

/// Every table with its columns (type, nullability, default), every
/// constraint, index, trigger, function and sequence of schema `public`, one
/// line each.
async fn catalog(db: &DatabaseConnection) -> BTreeSet<String> {
    let mut facts = BTreeSet::new();
    for sql in [
        "SELECT format('column %s.%s %s %s default %s', c.relname, a.attname, \
                format_type(a.atttypid, a.atttypmod), \
                CASE WHEN a.attnotnull THEN 'not null' ELSE 'null' END, \
                coalesce(pg_get_expr(d.adbin, d.adrelid), '-')) AS fact \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE n.nspname = 'public' AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped",
        "SELECT format('constraint %s.%s %s', c.relname, k.conname, pg_get_constraintdef(k.oid)) \
           AS fact \
         FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'",
        "SELECT format('index %s', indexdef) AS fact FROM pg_indexes WHERE schemaname = 'public'",
        "SELECT format('trigger %s', pg_get_triggerdef(t.oid)) AS fact \
         FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND NOT t.tgisinternal",
        "SELECT format('function %s', pg_get_functiondef(p.oid)) AS fact \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         LEFT JOIN pg_depend e ON e.objid = p.oid AND e.deptype = 'e' \
         WHERE n.nspname = 'public' AND e.objid IS NULL",
        "SELECT format('sequence %s', sequencename) AS fact FROM pg_sequences \
         WHERE schemaname = 'public'",
    ] {
        facts.extend(rows(db, sql).await);
    }
    facts
}

/// Each column of every table the entities define, as `table.column type
/// nullability`.
async fn entity_columns(db: &DatabaseConnection, tables: &BTreeSet<String>) -> BTreeSet<String> {
    rows(
        db,
        "SELECT format('%s.%s %s %s', c.relname, a.attname, \
                format_type(a.atttypid, a.atttypmod), \
                CASE WHEN a.attnotnull THEN 'not null' ELSE 'null' END) AS fact \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .await
    .into_iter()
    .filter(|fact| tables.contains(fact.split('.').next().unwrap_or_default()))
    .collect()
}

/// Build, from the code alone, every table some node reads through an entity:
/// the server's entities by schema sync, and the blob store and task cache
/// tables the storage layer and the workers use.
async fn create_from_entities(db: &DatabaseConnection) {
    let schema = db.as_sea_orm();
    schema
        .get_schema_registry("server::entity::*")
        .sync(schema)
        .await
        .expect("schema sync");
    let builder = Schema::new(DbBackend::Postgres);
    for mut table in [
        builder.create_table_from_entity(common::storage::database::blob_data::Entity),
        builder.create_table_from_entity(worker::models::operation::task_cache::task_cache::Entity),
    ] {
        table.if_not_exists();
        db.execute(&table).await.expect("create an entity table");
    }
}

async fn table_names(db: &DatabaseConnection) -> BTreeSet<String> {
    rows(
        db,
        "SELECT tablename::text AS fact FROM pg_tables WHERE schemaname = 'public'",
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_migrations_create_the_schema_the_code_expects() {
    let (_pg, port) = postgres().await;
    let admin = connect(&url(port, "postgres")).await;
    admin
        .execute_unprepared("CREATE DATABASE from_entities")
        .await
        .expect("create the entity database");

    let migrated_url = url(port, "postgres");
    let boot = BootSession::acquire(&migrated_url)
        .await
        .expect("boot session");
    boot.migrate().await.expect("migrate an empty database");
    let migrated = boot.conn();
    let before = catalog(migrated).await;

    // Everything the code would create is already there, exactly: schema
    // sync adds no table, column, key or index, and the trigger definitions
    // the listeners rely on are the ones the migrations installed.
    create_from_entities(migrated).await;
    crate::services::judging::install_claim_notify(migrated)
        .await
        .expect("claim trigger");
    crate::services::plugin_config_cache::expected::install(migrated)
        .await
        .expect("config epoch trigger");
    let after = catalog(migrated).await;
    let added: Vec<_> = after.difference(&before).collect();
    let removed: Vec<_> = before.difference(&after).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "the migrated schema differs from what the code expects; add a migration.\n\
         the code adds: {added:#?}\nthe code removes: {removed:#?}"
    );

    // Sync adds what is missing but never changes an existing column, so
    // compare each entity column's type and nullability with a database the
    // entities alone created.
    let entities = connect(&url(port, "from_entities")).await;
    create_from_entities(&entities).await;
    let tables = table_names(&entities).await;
    let expected = entity_columns(&entities, &tables).await;
    let actual = entity_columns(migrated, &tables).await;
    let missing: Vec<_> = expected.difference(&actual).collect();
    let unexpected: Vec<_> = actual.difference(&expected).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "entity columns and migrated columns differ; add a migration.\n\
         the entities have: {missing:#?}\nthe migrations have: {unexpected:#?}"
    );
    boot.release().await.expect("release");
}

/// The queue scans put their predicate inline so the planner can use the
/// partial claim indexes; an index whose predicate differs is never used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_claim_indexes_match_the_predicates_the_queue_scans_use() {
    let (_pg, port) = postgres().await;
    let boot = BootSession::acquire(&url(port, "postgres"))
        .await
        .expect("boot session");
    boot.migrate().await.expect("migrate");
    let db = boot.conn();
    let predicate = |sql: String| async move {
        db.query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .expect("predicate query")
            .expect("row")
            .try_get::<String>("", "fact")
            .expect("predicate")
    };
    let in_flight = format!("status IN {}", StatusPhase::InFlight.sql_list());
    for (index, table, expected) in [
        (
            "idx_submission_judgement_claim",
            "submission_judgement",
            crate::dispatcher::QUEUED_PREDICATE.to_string(),
        ),
        (
            "idx_code_run_claim",
            "code_run",
            crate::dispatcher::QUEUED_PREDICATE.to_string(),
        ),
        (
            "idx_submission_queued",
            "submission",
            crate::dispatcher::QUEUED_PREDICATE.to_string(),
        ),
        ("idx_code_run_in_flight", "code_run", in_flight),
    ] {
        let probe = format!("probe_{index}");
        db.execute_unprepared(&format!(
            "CREATE TEMP TABLE {probe} (LIKE {table}); \
             CREATE INDEX {probe}_idx ON {probe} (created_at) WHERE {expected}"
        ))
        .await
        .expect("probe index");
        let want = predicate(format!(
            "SELECT pg_get_expr(indpred, indrelid) AS fact FROM pg_index \
             WHERE indexrelid = '{probe}_idx'::regclass"
        ))
        .await;
        let have = predicate(format!(
            "SELECT pg_get_expr(indpred, indrelid) AS fact FROM pg_index \
             WHERE indexrelid = '{index}'::regclass"
        ))
        .await;
        assert_eq!(have, want, "{index}");
    }
    boot.release().await.expect("release");
}
