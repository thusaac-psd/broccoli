use broccoli_server_sdk::permissions as perm;
use common::storage::BlobStore;
use sea_orm::*;
use sea_query::{Expr, Index, IndexCreateStatement, PostgresQueryBuilder, Query, SelectStatement};
use tracing::{error, info};

use crate::entity::{
    additional_file, clarification, dead_letter_message, plugin_storage, problem_attachment, role,
    role_permission, submission, submission_judgement, test_case, test_case_result, user,
    user_role,
};
use crate::utils::hash;
use crate::utils::test_case_body::{INLINE_TEST_CASE_BODY_THRESHOLD_BYTES, prepare_test_case_body};

const DEFAULT_ROLES: &[&str] = &["admin", "problem_setter", "contestant"];
const SERVER_MIGRATION_PLUGIN_ID: &str = "__server_migration";
const SERVER_MIGRATION_COLLECTION: &str = "default";
const LARGE_TEST_CASE_BODY_BACKFILL_KEY: &str = "large_test_case_body_backfill_v1";
const LARGE_TEST_CASE_BODY_BACKFILL_BATCH_SIZE: u64 = 2;

const DEFAULT_MAPPINGS: &[(&str, &str)] = &[
    ("admin", perm::SUBMISSION_SUBMIT),
    ("admin", perm::SUBMISSION_VIEW_ALL),
    ("admin", perm::SUBMISSION_REJUDGE),
    ("admin", perm::PROBLEM_CREATE),
    ("admin", perm::PROBLEM_EDIT),
    ("admin", perm::PROBLEM_DELETE),
    ("admin", perm::CONTEST_CREATE),
    ("admin", perm::CONTEST_MANAGE),
    ("admin", perm::CONTEST_DELETE),
    ("admin", perm::USER_MANAGE),
    ("admin", perm::ROLE_MANAGE),
    ("admin", perm::PLUGIN_MANAGE),
    ("admin", perm::DLQ_MANAGE),
    ("admin", perm::SYSTEM_VIEW),
    ("admin", perm::SYSTEM_ADMIN),
    ("problem_setter", perm::SUBMISSION_SUBMIT),
    ("problem_setter", perm::SUBMISSION_VIEW_ALL),
    ("problem_setter", perm::PROBLEM_CREATE),
    ("problem_setter", perm::PROBLEM_EDIT),
    ("contestant", perm::SUBMISSION_SUBMIT),
];

pub async fn seed_role_permissions(db: &DatabaseConnection) -> Result<(), DbErr> {
    let mut roles_inserted = 0u32;
    for &name in DEFAULT_ROLES {
        let model = role::ActiveModel {
            name: Set(name.to_string()),
        };

        let result = role::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(role::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await;

        match result {
            Ok(_) => roles_inserted += 1,
            Err(DbErr::RecordNotInserted) => {}
            Err(e) => return Err(e),
        }
    }

    if roles_inserted > 0 {
        info!("Seeded {} new roles", roles_inserted);
    }

    let mut perms_inserted = 0u32;
    for &(role, permission) in DEFAULT_MAPPINGS {
        let model = role_permission::ActiveModel {
            role: Set(role.to_string()),
            permission: Set(permission.to_string()),
        };

        let result = role_permission::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    role_permission::Column::Role,
                    role_permission::Column::Permission,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(db)
            .await;

        match result {
            Ok(_) => perms_inserted += 1,
            Err(DbErr::RecordNotInserted) => {}
            Err(e) => return Err(e),
        }
    }

    if perms_inserted > 0 {
        info!("Seeded {} new role-permission mappings", perms_inserted);
    }

    Ok(())
}

pub async fn ensure_bootstrap_admin(
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<(), DbErr> {
    let username = username.trim();
    if username.is_empty() || password.is_empty() {
        return Ok(());
    }

    if let Some(existing) = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .filter(user::Column::DeletedAt.is_null())
        .one(db)
        .await?
    {
        let password_hash = hash::hash_password(password)
            .map_err(|e| DbErr::Custom(format!("failed to hash bootstrap admin password: {e}")))?;
        let mut active: user::ActiveModel = existing.clone().into();
        active.password = Set(password_hash);
        active.update(db).await?;
        ensure_user_role(db, existing.id, "admin").await?;
        info!(username, "Ensured bootstrap admin user");
        return Ok(());
    }

    let password_hash = hash::hash_password(password)
        .map_err(|e| DbErr::Custom(format!("failed to hash bootstrap admin password: {e}")))?;

    let txn = db.begin().await?;
    let created = user::ActiveModel {
        username: Set(username.to_string()),
        display_name: Set(username.to_string()),
        password: Set(password_hash),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&txn)
    .await?;

    ensure_user_role(&txn, created.id, "admin").await?;
    txn.commit().await?;

    info!(username, "Created bootstrap admin user");
    Ok(())
}

async fn ensure_user_role<C>(db: &C, user_id: i32, role_name: &str) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    let model = user_role::ActiveModel {
        user_id: Set(user_id),
        role: Set(role_name.to_string()),
    };

    let result = user_role::Entity::insert(model)
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                user_role::Column::UserId,
                user_role::Column::Role,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
        .await;

    match result {
        Ok(_) | Err(DbErr::RecordNotInserted) => Ok(()),
        Err(e) => Err(e),
    }
}

/// One index created at boot. A UNIQUE index enforces an invariant the
/// application relies on, so it carries the query that lists the rows violating
/// it, built from the same table, columns and filter as the index itself.
struct BootIndex {
    create: IndexCreateStatement,
    duplicates: Option<SelectStatement>,
}

impl BootIndex {
    /// A performance aid: failing to create it only costs speed.
    fn plain(create: &mut IndexCreateStatement) -> Self {
        Self {
            create: create.to_owned(),
            duplicates: None,
        }
    }

    /// A UNIQUE index on `cols` of `E`, optionally partial (`filter`).
    fn unique<E: EntityTrait>(name: &str, cols: &[E::Column], filter: Option<Expr>) -> Self {
        let mut create = Index::create();
        create.unique().name(name).table(E::default());
        let mut duplicates = Query::select();
        duplicates
            .columns(cols.iter().copied())
            .expr_as(Expr::cust("COUNT(*)"), "rows")
            .from(E::default())
            .group_by_columns(cols.iter().copied())
            .and_having(Expr::cust("COUNT(*) > 1"));
        for &col in cols {
            create.col(col);
        }
        if let Some(filter) = filter {
            create.and_where(filter.clone());
            duplicates.and_where(filter);
        }
        Self {
            create,
            duplicates: Some(duplicates),
        }
    }

    fn name(&self) -> &str {
        self.create
            .get_index_spec()
            .get_name()
            .unwrap_or("<unnamed>")
    }
}

/// Indexes the entity schema sync does not declare. UNIQUE ones enforce
/// invariants the application relies on (one active account per username, one
/// current judgement per submission, ...); the rest are performance aids.
fn boot_indexes() -> Vec<BootIndex> {
    vec![
        BootIndex::plain(
            Index::create()
                .name("idx_submission_user_created")
                .table(submission::Entity)
                .col(submission::Column::UserId)
                .col(submission::Column::CreatedAt),
        ),
        BootIndex::plain(
            Index::create()
                .name("idx_dlq_resolved_created")
                .table(dead_letter_message::Entity)
                .col(dead_letter_message::Column::Resolved)
                .col(dead_letter_message::Column::CreatedAt),
        ),
        BootIndex::unique::<user::Entity>(
            "idx_user_username_active",
            &[user::Column::Username],
            Some(Expr::col(user::Column::DeletedAt).is_null()),
        ),
        BootIndex::unique::<problem_attachment::Entity>(
            "idx_problem_attachment_problem_path_unique",
            &[
                problem_attachment::Column::ProblemId,
                problem_attachment::Column::Path,
            ],
            None,
        ),
        BootIndex::plain(
            Index::create()
                .name("idx_problem_attachment_problem_id")
                .table(problem_attachment::Entity)
                .col(problem_attachment::Column::ProblemId),
        ),
        BootIndex::unique::<additional_file::Entity>(
            "idx_additional_file_problem_lang_path_unique",
            &[
                additional_file::Column::ProblemId,
                additional_file::Column::Language,
                additional_file::Column::Path,
            ],
            None,
        ),
        BootIndex::plain(
            Index::create()
                .name("idx_additional_file_problem_id")
                .table(additional_file::Entity)
                .col(additional_file::Column::ProblemId),
        ),
        BootIndex::plain(
            Index::create()
                .name("idx_clarification_contest_created")
                .table(clarification::Entity)
                .col(clarification::Column::ContestId)
                .col(clarification::Column::CreatedAt),
        ),
        BootIndex::unique::<submission_judgement::Entity>(
            "idx_submission_judgement_version_unique",
            &[
                submission_judgement::Column::SubmissionId,
                submission_judgement::Column::Version,
            ],
            None,
        ),
        BootIndex::unique::<submission_judgement::Entity>(
            "idx_submission_judgement_one_current",
            &[submission_judgement::Column::SubmissionId],
            Some(Expr::col(submission_judgement::Column::IsCurrent)),
        ),
        BootIndex::plain(
            Index::create()
                .name("idx_test_case_result_judgement")
                .table(test_case_result::Entity)
                .col(test_case_result::Column::JudgementId),
        ),
    ]
}

/// Create every `boot_indexes` entry that is missing.
///
/// A UNIQUE index that cannot be created means its invariant is not enforced,
/// almost always because duplicate rows already exist (e.g. a duplicate
/// bootstrap admin from the old first-boot race). Boot then fails with the
/// index name and the SQL that lists the offending rows, so the operator can
/// remove them (see the upgrade guide). `allow_missing_unique` (config
/// `database.allow_missing_unique_indexes`) downgrades that failure to an error
/// log for one release, to let an existing deployment start before cleanup.
/// Callers run this under the boot lock (see
/// [`crate::database::BootSession`]), so a failure is never a concurrent-create
/// race with a peer. A failed performance index only costs speed and is logged.
pub async fn ensure_indexes(
    db: &DatabaseConnection,
    allow_missing_unique: bool,
) -> Result<(), DbErr> {
    for mut index in boot_indexes() {
        index.create.if_not_exists();
        ensure_index(db, &index, allow_missing_unique).await?;
    }
    Ok(())
}

async fn ensure_index(
    db: &DatabaseConnection,
    index: &BootIndex,
    allow_missing_unique: bool,
) -> Result<(), DbErr> {
    let name = index.name();
    let err = match db
        .execute_unprepared(&index.create.to_string(PostgresQueryBuilder))
        .await
    {
        Ok(_) => {
            info!(index = name, "Ensured index exists");
            return Ok(());
        }
        Err(e) => e,
    };
    let Some(duplicates) = &index.duplicates else {
        tracing::warn!(index = name, error = %err, "Failed to create index");
        return Ok(());
    };
    let find = duplicates.to_string(PostgresQueryBuilder);
    error!(
        index = name,
        error = %err,
        find_duplicates = %find,
        allow_missing_unique,
        "Failed to create a UNIQUE index, so its invariant is not enforced. List the \
         duplicate rows with the find_duplicates query, remove or rename them, then restart"
    );
    if allow_missing_unique {
        return Ok(());
    }
    Err(DbErr::Custom(format!(
        "failed to create unique index {name}: {err}. Find the duplicate rows with: {find}; \
         to start anyway for one release, set database.allow_missing_unique_indexes = true \
         (BROCCOLI__DATABASE__ALLOW_MISSING_UNIQUE_INDEXES=true)"
    )))
}

/// One-shot migration that ensures every existing submission has a v1
/// judgement and that every existing test_case_result row points at it.
///
/// Idempotent: rows that are already attached to a judgement are left
/// alone. Submissions that already have at least one judgement are left
/// alone. Safe to run on every server boot.
pub async fn backfill_submission_judgements(db: &DatabaseConnection) -> Result<(), DbErr> {
    let pending_subs: Option<i64> = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS count FROM submission s \
             WHERE NOT EXISTS (SELECT 1 FROM submission_judgement j WHERE j.submission_id = s.id)",
        ))
        .await?
        .map(|row| row.try_get::<i64>("", "count"))
        .transpose()?;
    let pending_results: Option<i64> = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS count FROM test_case_result WHERE judgement_id IS NULL",
        ))
        .await?
        .map(|row| row.try_get::<i64>("", "count"))
        .transpose()?;

    if pending_subs.unwrap_or(0) == 0 && pending_results.unwrap_or(0) == 0 {
        return Ok(());
    }

    info!(
        pending_submissions = pending_subs.unwrap_or(0),
        pending_results = pending_results.unwrap_or(0),
        "Backfilling submission_judgement rows for legacy submissions"
    );

    db.execute_unprepared(
        "INSERT INTO submission_judgement \
            (submission_id, version, is_current, is_finalized, \
             status, verdict, score, time_used, memory_used, \
             compile_output, error_code, error_message, judge_epoch, \
             created_at, finalized_at) \
         SELECT s.id, 1, TRUE, \
                s.status IN ('Judged', 'CompilationError', 'SystemError'), \
                s.status, s.verdict, s.score, s.time_used, s.memory_used, \
                s.compile_output, s.error_code, s.error_message, \
                COALESCE(s.judge_epoch, 0), \
                s.created_at, \
                CASE WHEN s.status IN ('Judged', 'CompilationError', 'SystemError') \
                     THEN COALESCE(s.judged_at, s.created_at) \
                     ELSE NULL END \
         FROM submission s \
         WHERE NOT EXISTS \
            (SELECT 1 FROM submission_judgement j WHERE j.submission_id = s.id)",
    )
    .await?;

    db.execute_unprepared(
        "UPDATE test_case_result tcr \
         SET judgement_id = j.id \
         FROM submission_judgement j \
         WHERE tcr.judgement_id IS NULL \
           AND j.submission_id = tcr.submission_id \
           AND j.is_current = TRUE",
    )
    .await?;

    info!("Backfill of submission_judgement complete");
    Ok(())
}

pub async fn backfill_large_test_case_bodies(
    db: &DatabaseConnection,
    blob_store: std::sync::Arc<dyn BlobStore>,
) -> Result<(), DbErr> {
    if plugin_storage::Entity::find_by_id((
        SERVER_MIGRATION_PLUGIN_ID.to_string(),
        SERVER_MIGRATION_COLLECTION.to_string(),
        LARGE_TEST_CASE_BODY_BACKFILL_KEY.to_string(),
    ))
    .one(db)
    .await?
    .is_some()
    {
        return Ok(());
    }

    let mut backfilled = 0usize;

    loop {
        let rows = test_case::Entity::find()
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(test_case::Column::InputBlobHash.is_null())
                            .add(Expr::cust(format!(
                                "octet_length(\"input\") >= {INLINE_TEST_CASE_BODY_THRESHOLD_BYTES}"
                            ))),
                    )
                    .add(
                        Condition::all()
                            .add(test_case::Column::ExpectedOutputBlobHash.is_null())
                            .add(Expr::cust(format!(
                                "octet_length(\"expected_output\") >= {INLINE_TEST_CASE_BODY_THRESHOLD_BYTES}"
                            ))),
                    ),
            )
            .limit(LARGE_TEST_CASE_BODY_BACKFILL_BATCH_SIZE)
            .all(db)
            .await?;

        if rows.is_empty() {
            break;
        }

        for row in rows {
            let mut active: test_case::ActiveModel = row.clone().into();
            let mut changed = false;

            if row.input_blob_hash.is_none()
                && row.input.len() >= INLINE_TEST_CASE_BODY_THRESHOLD_BYTES
            {
                let body = prepare_test_case_body(row.input.clone(), blob_store.clone())
                    .await
                    .map_err(|e| DbErr::Custom(format!("failed to backfill input blob: {e:?}")))?;
                active.input = Set(body.inline_text);
                active.input_blob_hash = Set(body.blob_hash);
                active.input_size = Set(Some(body.size));
                active.input_preview = Set(Some(body.preview));
                changed = true;
            }

            if row.expected_output_blob_hash.is_none()
                && row.expected_output.len() >= INLINE_TEST_CASE_BODY_THRESHOLD_BYTES
            {
                let body = prepare_test_case_body(row.expected_output.clone(), blob_store.clone())
                    .await
                    .map_err(|e| DbErr::Custom(format!("failed to backfill output blob: {e:?}")))?;
                active.expected_output = Set(body.inline_text);
                active.expected_output_blob_hash = Set(body.blob_hash);
                active.expected_output_size = Set(Some(body.size));
                active.expected_output_preview = Set(Some(body.preview));
                changed = true;
            }

            if changed {
                active.update(db).await?;
                backfilled += 1;
            }
        }
    }

    if backfilled > 0 {
        info!(
            rows = backfilled,
            "Backfilled large test case bodies to blob storage"
        );
    }

    plugin_storage::Entity::insert(plugin_storage::ActiveModel {
        revision: Set(0),
        plugin_id: Set(SERVER_MIGRATION_PLUGIN_ID.to_string()),
        collection: Set(SERVER_MIGRATION_COLLECTION.to_string()),
        key: Set(LARGE_TEST_CASE_BODY_BACKFILL_KEY.to_string()),
        data: Set(serde_json::json!({
            "completed_at": chrono::Utc::now(),
            "rows": backfilled,
        })),
        created_at: Set(chrono::Utc::now()),
    })
    .on_conflict(
        sea_query::OnConflict::columns([
            plugin_storage::Column::PluginId,
            plugin_storage::Column::Collection,
            plugin_storage::Column::Key,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(db)
    .await?;

    Ok(())
}

pub fn spawn_large_test_case_body_backfill(
    db: DatabaseConnection,
    blob_store: std::sync::Arc<dyn BlobStore>,
) {
    tokio::spawn(async move {
        if let Err(e) = backfill_large_test_case_bodies(&db, blob_store).await {
            error!(
                error = %e,
                "Background large test case body backfill failed"
            );
        }
    });
}
