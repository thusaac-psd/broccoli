use broccoli_server_sdk::permissions as perm;
use common::db::DatabaseConnection;
use sea_orm::*;
use tracing::info;

use crate::config::BootstrapConfig;
use crate::entity::{refresh_token, role, role_permission, user, user_role};
use crate::services::privileges::SystemAdminLock;
use crate::utils::hash;
use crate::utils::soft_delete::SoftDeletable;

const ADMIN_ROLE: &str = "admin";
const DEFAULT_ROLES: &[&str] = &[ADMIN_ROLE, "problem_setter", "contestant"];

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

/// Apply each built-in mapping to `role_permission` exactly once, the first
/// time it is recorded in `seeded_role_permission`. A mapping an admin later
/// revokes stays revoked across restarts; a mapping added to
/// [`DEFAULT_MAPPINGS`] by a later release is applied on its first boot.
const SEED_MAPPINGS_SQL: &str = r#"
WITH mapping AS (
    SELECT role, permission
    FROM jsonb_to_recordset($1::jsonb) AS m(role text, permission text)
),
recorded AS (
    INSERT INTO seeded_role_permission (role, permission)
    SELECT role, permission FROM mapping
    ON CONFLICT DO NOTHING
    RETURNING role, permission
)
INSERT INTO role_permission (role, permission)
SELECT role, permission FROM recorded
ON CONFLICT DO NOTHING
"#;

/// Ensure the built-in roles exist and apply new built-in role-permission
/// mappings (see `SEED_MAPPINGS_SQL`).
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

    let mappings = serde_json::Value::Array(
        DEFAULT_MAPPINGS
            .iter()
            .map(|(role, permission)| serde_json::json!({ "role": role, "permission": permission }))
            .collect(),
    );
    let perms_inserted = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            SEED_MAPPINGS_SQL,
            [mappings.into()],
        ))
        .await?
        .rows_affected();

    if perms_inserted > 0 {
        info!("Seeded {} new role-permission mappings", perms_inserted);
    }

    Ok(())
}

/// Make sure some admin can sign in, without undoing an operator's later
/// changes on every boot of every replica.
///
/// Boot acts only when one of these holds; otherwise it changes nothing (a
/// password changed in the UI, a revoked role, or a renamed or deleted
/// bootstrap account all survive restarts):
/// - no active user holds `system:admin`, which includes a fresh database;
/// - `reset_admin` is set: an explicit request, for example after the admin
///   password was lost.
///
/// It then creates the bootstrap account if no active user has its username,
/// or else resets its password to the configured one and ends its sessions,
/// and in both cases grants the `admin` role and re-applies the built-in
/// `admin` permissions.
pub async fn ensure_bootstrap_admin(
    db: &DatabaseConnection,
    bootstrap: &BootstrapConfig,
) -> Result<(), DbErr> {
    let username = bootstrap.admin_username.trim();
    let password = bootstrap.admin_password.as_str();
    if username.is_empty() || password.is_empty() {
        return Ok(());
    }
    let hash_configured_password = || async {
        hash::hash_password_off_thread(password.to_owned())
            .await
            .map_err(|e| DbErr::Custom(format!("failed to hash bootstrap admin password: {e}")))
    };

    let txn = db.begin().await?;
    // Serializes with every write that can remove a `system:admin` holder, so
    // the holder check below cannot race one.
    let admins = SystemAdminLock::acquire(&txn).await.map_err(app_err)?;

    let reason = if bootstrap.reset_admin {
        "bootstrap.reset_admin is set"
    } else if !admins.any_holder().await.map_err(app_err)? {
        "no active user holds system:admin"
    } else {
        // An operator may have renamed or deleted the bootstrap account on
        // purpose; with an admin in place there is nothing to recover.
        info!(
            username,
            "An active system:admin holder exists; leaving the bootstrap admin unchanged"
        );
        return Ok(());
    };

    let existing = user::Entity::find_active()
        .filter(user::Column::Username.eq(username))
        .one(&txn)
        .await?;
    let user_id = match existing {
        Some(existing) => {
            let mut active: user::ActiveModel = existing.clone().into();
            active.password = Set(hash_configured_password().await?);
            active.update(&txn).await?;
            refresh_token::Entity::revoke_all_for_user(&txn, existing.id).await?;
            existing.id
        }
        None => {
            user::ActiveModel {
                username: Set(username.to_string()),
                display_name: Set(username.to_string()),
                password: Set(hash_configured_password().await?),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(&txn)
            .await?
            .id
        }
    };
    user_role::Entity::grant(&txn, user_id, ADMIN_ROLE).await?;
    for &(role, permission) in DEFAULT_MAPPINGS.iter().filter(|(r, _)| *r == ADMIN_ROLE) {
        role_permission::Entity::insert(role_permission::ActiveModel {
            role: Set(role.to_owned()),
            permission: Set(permission.to_owned()),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                role_permission::Column::Role,
                role_permission::Column::Permission,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;
    }
    user::Entity::touch_credentials_changed_for_role(&txn, ADMIN_ROLE).await?;
    txn.commit().await?;
    tracing::warn!(
        username,
        reason,
        "Bootstrap admin ready: password set, admin role and its built-in permissions restored"
    );
    Ok(())
}

fn app_err(e: crate::error::AppError) -> DbErr {
    DbErr::Custom(format!("{e:?}"))
}
