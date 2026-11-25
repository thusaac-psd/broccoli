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
use sea_orm::{ConnectionTrait, DbBackend, Schema, Statement, TransactionTrait};
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
    crate::services::judging::install_inputs_triggers(migrated)
        .await
        .expect("inputs version triggers");
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

/// `error_stands` arrives on judgements decided before it: every finalized
/// system error that no retry can take any more (one its submission already
/// shows, or one that is not current) stands; one still awaiting the retry's
/// decision (its submission shows the attempt in flight) does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_stands_marks_the_system_errors_decided_before_it() {
    use sea_orm_migration::MigratorTrait;

    let (_pg, port) = postgres().await;
    let db = connect(&url(port, "postgres")).await;
    let before = super::Migrator::migrations()
        .iter()
        .position(|m| m.name() == "m0010_error_stands")
        .expect("m0010 is known");
    super::Migrator::up(db.as_sea_orm(), Some(before as u32))
        .await
        .expect("migrate up to m0010");
    db.execute_unprepared(
        "INSERT INTO \"user\" (id, username, display_name, password, created_at) \
             VALUES (1, 'u', 'u', 'x', NOW()); \
         INSERT INTO problem (id, title, content, time_limit, memory_limit, problem_type, \
             default_contest_type, checker_format, created_at, updated_at) \
             VALUES (1, 'p', 'c', 1000, 1024, 'standard', 'standard', 'lines', NOW(), NOW()); \
         INSERT INTO submission (id, files, language, user_id, problem_id, contest_type, \
             status, verdict, judge_epoch, max_judge_epoch, created_at) VALUES \
             (1, '[]', 'cpp', 1, 1, 'standard', 'SystemError', 'SystemError', 2, 2, NOW()), \
             (2, '[]', 'cpp', 1, 1, 'standard', 'Running', NULL, 2, 2, NOW()); \
         INSERT INTO submission_judgement (id, submission_id, version, is_current, \
             is_finalized, status, verdict, judge_epoch, created_at, queued_at) VALUES \
             (1, 1, 2, TRUE, TRUE, 'SystemError', 'SystemError', 2, NOW(), NOW()), \
             (2, 1, 1, FALSE, TRUE, 'Judged', 'SystemError', 1, NOW(), NOW()), \
             (3, 2, 1, TRUE, TRUE, 'SystemError', 'SystemError', 2, NOW(), NOW()), \
             (4, 2, 0, FALSE, TRUE, 'Judged', 'Accepted', 1, NOW(), NOW());",
    )
    .await
    .expect("seed judgements decided before m0010");
    super::Migrator::up(db.as_sea_orm(), None)
        .await
        .expect("migrate the rest");

    let standing = rows(
        &db,
        "SELECT id::text AS fact FROM submission_judgement WHERE error_stands",
    )
    .await;
    assert_eq!(
        standing,
        BTreeSet::from(["1".to_string(), "2".to_string()]),
        "the shown and the past system errors stand; the undecided one and a verdict do not"
    );
}

/// The last migration of the release installations run (`master`). Every
/// migration after it is part of this release's upgrade.
const RELEASED: &str = "m0009_config_issues";

/// A database the released build ran, with rows in that build's shapes:
/// users with sessions, refresh tokens and device codes, a plugin group role,
/// enabled and disabled plugins with what they governed and a replica's
/// heartbeat, a problem, a contest, submissions and judgements, a code run
/// and a stored config. Migrated to the released schema first.
async fn released_database(db: &DatabaseConnection) {
    use sea_orm_migration::MigratorTrait;

    let released = super::Migrator::migrations()
        .iter()
        .position(|m| m.name() == RELEASED)
        .expect("the released migration is known");
    super::Migrator::up(db.as_sea_orm(), Some(released as u32 + 1))
        .await
        .expect("migrate to the released schema");
    db.execute_unprepared(
        "DO $$ BEGIN \
             IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'released_plugins') THEN \
                 CREATE ROLE released_plugins NOLOGIN; \
             END IF; \
         END $$; \
         INSERT INTO plugin_group_role (name) VALUES ('released_plugins'); \
         GRANT SELECT (id, error_message) ON submission, submission_judgement, code_run \
             TO released_plugins; \
         INSERT INTO \"user\" (id, username, display_name, password, created_at) VALUES \
             (1, 'alice', 'Alice', 'x', NOW()), (2, 'bob', 'Bob', 'x', NOW()); \
         INSERT INTO auth_session (id, user_id, created_at) VALUES (1, 1, NOW()); \
         INSERT INTO refresh_tokens (selector, validator, user_id, expires_at, created_at, \
             session_id) VALUES \
             ('s1', 'v', 1, NOW() + interval '1 day', NOW(), 1), \
             ('s2', 'v', 2, NOW() + interval '1 day', NOW(), NULL); \
         INSERT INTO device_authorization (device_code_hash, user_code, authorized_user_id, \
             created_at, expires_at) VALUES \
             ('pending', 'AAAA-AAAA', NULL, NOW(), NOW() + interval '10 minutes'), \
             ('approved', 'BBBB-BBBB', 1, NOW(), NOW() + interval '10 minutes'); \
         INSERT INTO plugin (id, is_enabled, generation, version, governs, updated_at) VALUES \
             ('icpc', TRUE, 3, '1.0.0', \
                 '{\"visibility\": false, \"contest_types\": [\"icpc\"]}', NOW()), \
             ('ioi', FALSE, 2, '1.0.0', \
                 '{\"visibility\": true, \"contest_types\": [\"ioi\"]}', NOW()), \
             ('notes', TRUE, 1, '0.1.0', NULL, NOW()), \
             ('odd', TRUE, 1, '0.1.0', '[\"icpc\"]', NOW()); \
         INSERT INTO plugin_replica (server_id, plugins, contest_types, state_at, \
             heartbeat_at) VALUES \
             ('server-a', '{\"icpc\": \"1.0.0\"}', '[\"icpc\"]', NOW(), NOW()); \
         INSERT INTO plugin_config (scope, ref_id, namespace, config, enabled) VALUES \
             ('problem', '1', 'icpc', '{}', TRUE); \
         INSERT INTO problem (id, title, content, time_limit, memory_limit, problem_type, \
             default_contest_type, checker_format, created_at, updated_at) VALUES \
             (1, 'p', 'c', 1000, 1024, 'batch', 'icpc', 'exact', NOW(), NOW()); \
         INSERT INTO contest (id, title, description, start_time, end_time, contest_type, \
             created_at, updated_at) VALUES \
             (1, 'c', 'd', NOW(), NOW() + interval '1 hour', 'icpc', NOW(), NOW()); \
         INSERT INTO contest_problem (contest_id, problem_id, label) VALUES (1, 1, 'A'); \
         INSERT INTO contest_user (contest_id, user_id, registered_at) VALUES (1, 2, NOW()); \
         INSERT INTO submission (id, files, language, user_id, problem_id, contest_id, \
             contest_type, status, verdict, judge_epoch, max_judge_epoch, created_at) VALUES \
             (1, '[]', 'cpp', 2, 1, 1, 'icpc', 'Judged', 'Accepted', 1, 1, NOW()), \
             (2, '[]', 'cpp', 2, 1, 1, 'icpc', 'SystemError', 'SystemError', 1, 1, NOW()), \
             (3, '[]', 'cpp', 2, 1, NULL, 'ioi', 'Queued', NULL, 0, 0, NOW()); \
         INSERT INTO submission_judgement (id, submission_id, version, is_current, \
             is_finalized, status, verdict, judge_epoch, created_at, queued_at) VALUES \
             (1, 1, 1, TRUE, TRUE, 'Judged', 'Accepted', 1, NOW(), NOW()), \
             (2, 2, 1, TRUE, TRUE, 'SystemError', 'SystemError', 1, NOW(), NOW()); \
         INSERT INTO code_run (id, files, language, user_id, problem_id, contest_type, status, \
             custom_test_cases, created_at) VALUES \
             (1, '[]', 'cpp', 2, 1, 'icpc', 'Judged', '[]', NOW());",
    )
    .await
    .expect("seed the released build's rows");
}

/// An installation upgrades from the released build: boot migrates a database
/// that build ran, with its rows in its shapes, to this build's schema, and
/// every row then reads through the code's entities. A pending migration that
/// assumes an empty table fails here, not on an operator's upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_the_released_build_ran_upgrades_to_this_build() {
    use crate::entity;
    use crate::plugin_lifecycle::Governs;
    use sea_orm::EntityTrait;

    let (_pg, port) = postgres().await;
    let db_url = url(port, "postgres");
    released_database(&connect(&db_url).await).await;

    let boot = BootSession::acquire(&db_url).await.expect("boot session");
    boot.migrate()
        .await
        .expect("boot migrates a database the released build ran");
    let db = boot.conn().as_sea_orm();

    let plugins = entity::plugin::Entity::find()
        .all(db)
        .await
        .expect("plugins");
    assert_eq!(plugins.len(), 4);
    for plugin in &plugins {
        if let Some(governs) = &plugin.governs {
            serde_json::from_value::<Governs>(governs.clone()).unwrap_or_else(|e| {
                panic!("{}'s governs reads as this build's shape: {e}", plugin.id)
            });
        }
    }
    let ioi = plugins.iter().find(|p| p.id == "ioi").expect("ioi");
    let governs: Governs = serde_json::from_value(ioi.governs.clone().expect("kept")).unwrap();
    assert!(governs.visibility);
    assert_eq!(
        governs.provides.contest_types,
        ["ioi"],
        "a disabled plugin still closes the contest types it governed"
    );
    assert!(
        governs.provides.languages.is_empty()
            && governs.unchecked_problem_types.is_empty()
            && governs.hooks.is_empty()
            && governs.judging_inputs.is_empty(),
        "a key the released build did not record reads as none"
    );

    let odd = plugins.iter().find(|p| p.id == "odd").expect("odd");
    assert_eq!(
        odd.governs, None,
        "a row in no known shape is cleared for its next load to record"
    );

    macro_rules! reads {
        ($($table:ident),* $(,)?) => {$(
            entity::$table::Entity::find()
                .all(db)
                .await
                .unwrap_or_else(|e| panic!("{} rows read: {e}", stringify!($table)));
        )*};
    }
    reads!(
        user,
        auth_session,
        refresh_token,
        device_authorization,
        plugin_replica,
        plugin_config,
        problem,
        contest,
        contest_problem,
        contest_user,
        submission,
        submission_judgement,
        code_run,
    );

    // The judging-inputs triggers run on an upgraded row.
    let version = || async {
        entity::problem::Entity::find_by_id(1)
            .one(db)
            .await
            .expect("problem")
            .expect("kept")
            .inputs_version
    };
    let before = version().await;
    db.execute_unprepared("UPDATE problem SET time_limit = time_limit + 1 WHERE id = 1")
        .await
        .expect("change an upgraded problem's limits");
    assert!(
        version().await > before,
        "a change to an upgraded problem counts"
    );
    boot.release().await.expect("release");
}

/// Each migration of this release says in its docs whether it is additive
/// (`Additive` or `Not additive`), and the upgrade guide names every one that
/// is not, since such a release is upgraded with every server stopped.
#[test]
fn the_upgrade_guide_names_every_migration_of_this_release_that_is_not_additive() {
    use sea_orm_migration::MigratorTrait;

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migration");
    let guide = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../release/docs/upgrade.md"),
    )
    .expect("release/docs/upgrade.md");
    let names: Vec<String> = super::Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let released = names
        .iter()
        .position(|name| name == RELEASED)
        .expect("the released migration is known");
    for name in &names[released + 1..] {
        let source = std::fs::read_to_string(dir.join(format!("{name}.rs")))
            .unwrap_or_else(|e| panic!("{name}.rs: {e}"));
        let docs: String = source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("///"))
            .collect::<Vec<_>>()
            .join(" ");
        let not_additive = docs.contains("Not additive");
        assert!(
            not_additive || docs.contains("Additive"),
            "{name} does not say whether it is additive"
        );
        assert_eq!(
            guide.contains(&format!("`{name}`")),
            not_additive,
            "release/docs/upgrade.md must name exactly the migrations that are not additive; \
             {name} is {}",
            if not_additive {
                "not additive"
            } else {
                "additive"
            }
        );
    }
}

/// Every change to what judging reads of a problem counts in its
/// `inputs_version`, whatever path makes it: its limits, type and checker
/// format, its test cases, its additional files, the type of a contest it is
/// in, and the configs of a namespace its plugin declares a judging input at
/// every scope the cascade reads (the problem's, its contest problem's, its
/// contest's, the plugin's). A change to anything else (its title, a config
/// of a namespace judging does not read) does not.
#[tokio::test]
async fn every_change_to_a_problems_judging_inputs_counts_in_its_version() {
    use sea_orm::EntityTrait;
    let (_pg, db) = crate::test_support::postgres().await;
    let (_, problem_id) = crate::test_support::seed_user_and_problem(&db).await;
    let exec = |sql: String| {
        let db = db.clone();
        async move {
            db.execute_unprepared(&sql).await.unwrap();
        }
    };
    let version = || {
        let db = db.clone();
        async move {
            crate::entity::problem::Entity::find_by_id(problem_id)
                .one(&db)
                .await
                .unwrap()
                .unwrap()
                .inputs_version
        }
    };
    exec(format!(
        "INSERT INTO plugin (id, is_enabled, generation, updated_at, governs) VALUES \
           ('judge', TRUE, 0, NOW(), '{governs}'), ('display', TRUE, 0, NOW(), '{display}'); \
         INSERT INTO contest (id, title, description, start_time, end_time, contest_type, \
                              created_at, updated_at) \
         VALUES (7, 'c', '', NOW(), NOW() + interval '1 day', 'judge', NOW(), NOW()), \
                (8, 'd', '', NOW(), NOW() + interval '1 day', 'judge', NOW(), NOW()); \
         INSERT INTO contest_problem (contest_id, problem_id, label, position) \
         VALUES (7, {problem_id}, 'A', 0)",
        governs = serde_json::json!({
            "visibility": false, "contest_types": ["judge"], "problem_types": [],
            "checker_formats": [], "languages": [], "unchecked_problem_types": [],
            "hooks": [], "judging_inputs": ["checker"],
        }),
        display = serde_json::json!({
            "visibility": false, "contest_types": [], "problem_types": [],
            "checker_formats": [], "languages": [], "unchecked_problem_types": [],
            "hooks": [], "judging_inputs": [],
        }),
    ))
    .await;
    let mut last = version().await;
    let changes = [
        (
            "title",
            "UPDATE problem SET title = 'renamed' WHERE id = {p}".to_string(),
            false,
        ),
        (
            "time limit",
            "UPDATE problem SET time_limit = time_limit + 1 WHERE id = {p}".into(),
            true,
        ),
        (
            "memory limit",
            "UPDATE problem SET memory_limit = memory_limit + 1 WHERE id = {p}".into(),
            true,
        ),
        (
            "checker format",
            "UPDATE problem SET checker_format = 'exact' WHERE id = {p}".into(),
            true,
        ),
        (
            "problem type",
            "UPDATE problem SET problem_type = 'other' WHERE id = {p}".into(),
            true,
        ),
        (
            "test case added",
            "INSERT INTO test_case (input, expected_output, score, label, is_sample, position, \
                                    problem_id, created_at) \
             VALUES ('1', '1', 10, 't1', false, 0, {p}, NOW())"
                .into(),
            true,
        ),
        (
            "test case changed",
            "UPDATE test_case SET score = 20 WHERE problem_id = {p}".into(),
            true,
        ),
        (
            "test case removed",
            "DELETE FROM test_case WHERE problem_id = {p}".into(),
            true,
        ),
        (
            "additional file",
            "INSERT INTO additional_file (id, problem_id, language, path, content_hash, filename, \
                                          size, created_at) \
             VALUES (gen_random_uuid(), {p}, 'cpp', 'grader.cpp', 'h', 'grader.cpp', 1, NOW())"
                .into(),
            true,
        ),
        (
            "problem config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('problem', '{p}', 'judge:checker', '{{}}', 0, NOW())"
                .into(),
            true,
        ),
        (
            "problem config saved as it was",
            "UPDATE plugin_config SET config = config, updated_at = NOW() + interval '1 minute' \
             WHERE scope = 'problem' AND ref_id = '{p}'"
                .into(),
            false,
        ),
        (
            "problem config changed",
            "UPDATE plugin_config SET config = '{\"k\": 1}' \
             WHERE scope = 'problem' AND ref_id = '{p}'"
                .into(),
            true,
        ),
        (
            "contest-problem config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('contest_problem', '7:{p}', 'judge:checker', '{{}}', 0, NOW())"
                .into(),
            true,
        ),
        (
            "contest config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('contest', '7', 'judge:checker', '{{}}', 0, NOW())"
                .into(),
            true,
        ),
        (
            "another contest's config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('contest', '8', 'judge:checker', '{{}}', 0, NOW())"
                .into(),
            false,
        ),
        (
            "plugin config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('plugin', 'judge', 'checker', '{{}}', 0, NOW())"
                .into(),
            true,
        ),
        (
            "a config judging does not read",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('problem', '{p}', 'display:checker', '{{}}', 0, NOW()), \
                    ('contest', '7', 'judge:standings', '{{}}', 0, NOW()), \
                    ('plugin', 'display', 'checker', '{{}}', 0, NOW())"
                .into(),
            false,
        ),
        (
            "contest type",
            "UPDATE contest SET contest_type = 'other' WHERE id = 7".into(),
            true,
        ),
        (
            "another contest's type",
            "UPDATE contest SET contest_type = 'other' WHERE id = 8".into(),
            false,
        ),
        (
            "another problem's config",
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('problem', '999999', 'judge:checker', '{{}}', 0, NOW())"
                .into(),
            false,
        ),
    ];
    for (what, sql, counts) in changes {
        exec(
            sql.replace("{p}", &problem_id.to_string())
                .replace("{{}}", "{}"),
        )
        .await;
        let now = version().await;
        assert_eq!(now > last, counts, "{what}: {last} -> {now}");
        last = now;
    }

    // A restore (a bundle import) puts rows back as they were: it counts
    // nothing.
    let txn = db.begin().await.unwrap();
    txn.execute_unprepared(crate::services::judging::RESTORING)
        .await
        .unwrap();
    txn.execute_unprepared(&format!(
        "INSERT INTO test_case (input, expected_output, score, label, is_sample, position, \
                                problem_id, created_at) \
         VALUES ('2', '2', 10, 't2', false, 1, {problem_id}, NOW()); \
         UPDATE problem SET time_limit = time_limit + 1 WHERE id = {problem_id}"
    ))
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(version().await, last, "a restore counts nothing");
}
