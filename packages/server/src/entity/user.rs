use sea_orm::{Set, entity::prelude::*};
use serde::{Deserialize, Serialize};

use crate::utils::soft_delete::SoftDeletable;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub username: String,
    pub display_name: String,
    pub password: String,

    #[sea_orm(has_many, via = "user_role")]
    pub roles: HasMany<super::role::Entity>,

    #[sea_orm(has_many)]
    pub submissions: HasMany<super::submission::Entity>,

    #[sea_orm(has_many, via = "contest_user")]
    pub contests: HasMany<super::contest::Entity>,

    pub created_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,

    /// When this user's credentials/authorization last changed (password reset,
    /// role grant/revoke, deactivation). Access tokens minted before this
    /// instant are rejected by the `FreshAuthUser` extractor, so a downgraded or
    /// deactivated user cannot keep using a still-unexpired access token on
    /// high-value mutations.
    pub credentials_changed_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

impl SoftDeletable for Entity {
    type DeletedAtColumn = Column;
    fn deleted_at() -> Self::DeletedAtColumn {
        Column::DeletedAt
    }
}

impl Model {
    pub async fn assign_role<C>(self, db: &C, role_name: String) -> Result<(), sea_orm::DbErr>
    where
        C: ConnectionTrait,
    {
        super::user_role::ActiveModel {
            user_id: Set(self.id),
            role: Set(role_name),
        }
        .insert(db)
        .await?;
        Ok(())
    }
}

impl Entity {
    /// Stamp a single user's `credentials_changed_at` to now, invalidating any
    /// access token minted before this instant on the `FreshAuthUser` path.
    /// Call inside the same transaction as the credential/authorization change.
    pub async fn touch_credentials_changed<C>(db: &C, user_id: i32) -> Result<(), sea_orm::DbErr>
    where
        C: ConnectionTrait,
    {
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            r#"UPDATE "user" SET credentials_changed_at = now() WHERE id = $1"#,
            [user_id.into()],
        ))
        .await?;
        Ok(())
    }

    /// Stamp `credentials_changed_at` for every user holding `role_name`, so a
    /// change to that role's permission set invalidates the in-flight access
    /// tokens of everyone who currently has it.
    pub async fn touch_credentials_changed_for_role<C>(
        db: &C,
        role_name: &str,
    ) -> Result<(), sea_orm::DbErr>
    where
        C: ConnectionTrait,
    {
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            r#"UPDATE "user" SET credentials_changed_at = now()
               WHERE id IN (SELECT user_id FROM user_role WHERE role = $1)"#,
            [role_name.into()],
        ))
        .await?;
        Ok(())
    }
}
