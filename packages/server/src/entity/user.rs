use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::utils::soft_delete::SoftDeletable;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// As the user typed it, for display. Unique among active accounts by its
    /// [`username_key`], enforced by the partial unique index
    /// `idx_user_username_active` on `lower(username)`, so `Admin` cannot
    /// register next to `admin`.
    pub username: String,
    pub display_name: String,
    pub password: String,

    #[sea_orm(has_many, via = "user_role")]
    pub roles: HasMany<super::role::Entity>,

    #[sea_orm(has_many)]
    pub submissions: HasMany<super::submission::Entity>,

    #[sea_orm(has_many, via = "contest_user")]
    pub contests: HasMany<super::contest::Entity>,

    #[sea_orm(default_expr = "Expr::current_timestamp()")]
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

// The identity of a username: what two usernames must differ in to belong to
// different accounts. The rule is ASCII case folding of the name without its
// surrounding whitespace; usernames are ASCII (see
// `models::user::normalize_username`), so SQL `lower()` and Rust's ASCII
// lowercase agree on every name. The rule has two forms, kept side by side:
// [`username_key`] for names in Rust and [`username_key_sql`] for names in SQL.
// Every comparison of usernames goes through one of them: login, registration,
// bulk imports, contest participant imports, the bundle's account merge, and
// the active-username unique index (`idx_user_username_active`, on
// `lower(username)`).

/// The identity of `username` (see the rule above).
pub fn username_key(username: &str) -> String {
    username.trim().to_ascii_lowercase()
}

/// The identity of the username SQL expression `expr`, a column or a bound
/// value (see the rule above). Stored usernames are already trimmed.
pub fn username_key_sql(expr: &str) -> String {
    format!("lower({expr})")
}

/// Rows whose username has the same identity as one of `usernames`. Served by
/// the active-username index, whose expression is
/// `username_key_sql("username")`.
pub fn username_is_any<I, S>(usernames: I) -> sea_orm::sea_query::SimpleExpr
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    use sea_orm::sea_query::{Expr, ExprTrait};
    Expr::cust(username_key_sql(r#""user"."username""#))
        .is_in(usernames.into_iter().map(|u| username_key(u.as_ref())))
}

impl SoftDeletable for Entity {
    type DeletedAtColumn = Column;
    fn deleted_at() -> Self::DeletedAtColumn {
        Column::DeletedAt
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
