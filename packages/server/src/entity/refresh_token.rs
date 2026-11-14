use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "refresh_tokens")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub selector: String,
    pub validator: String,

    pub user_id: i32,

    pub expires_at: DateTimeUtc,
    pub created_at: DateTimeUtc,

    /// When this token was rotated out and superseded. `None` for a live token.
    /// A rotated token is retained (with this set) rather than deleted so that a
    /// replay of an already-rotated token can be detected as reuse (a stolen
    /// refresh chain) instead of being indistinguishable from a random invalid
    /// selector. Rows are cleaned up once expired.
    #[sea_orm(nullable)]
    pub revoked_at: Option<DateTimeUtc>,

    /// Selector of the row this one replaced, when it was issued by a
    /// rotation. It caps the reuse grace at one sibling per rotated row.
    #[sea_orm(nullable, indexed)]
    pub rotated_from: Option<String>,

    /// The sign-in this token belongs to. Deleting the session deletes its
    /// tokens. A row without one is not a credential: presenting it is
    /// `TOKEN_INVALID`.
    #[sea_orm(nullable, indexed)]
    pub session_id: Option<i64>,

    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,

    #[sea_orm(belongs_to, from = "session_id", to = "id", on_delete = "Cascade")]
    pub session: HasOne<super::auth_session::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}

/// Row lock on the owning user that every refresh-token write for that user
/// takes first: a rotation (`services::auth_tokens::rotate_refresh_token`) and
/// [`Entity::revoke_all_for_user`]. It orders the two, so a rotation that
/// commits while a password reset is deleting the user's rows cannot leave
/// behind a fresh row the reset's DELETE did not see. `NO KEY UPDATE` does not
/// block inserts into tables that reference the user.
pub const LOCK_OWNER_SQL: &str = r#"SELECT 1 FROM "user" WHERE id = $1 FOR NO KEY UPDATE"#;

/// Delete a user's sessions (their refresh tokens cascade) and any refresh
/// token of the user outside a session.
const END_USER_SESSIONS_SQL: &str = r#"
WITH sessions AS (DELETE FROM auth_session WHERE user_id = $1)
DELETE FROM refresh_tokens WHERE user_id = $1 AND session_id IS NULL
"#;

impl Entity {
    /// End every session of `user_id`, with all its refresh tokens, after
    /// taking the owner lock (see [`LOCK_OWNER_SQL`]). The user's access
    /// tokens fail the freshness check from then on.
    pub async fn revoke_all_for_user<C>(db: &C, user_id: i32) -> Result<(), sea_orm::DbErr>
    where
        C: ConnectionTrait,
    {
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            LOCK_OWNER_SQL,
            [user_id.into()],
        ))
        .await?;
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            END_USER_SESSIONS_SQL,
            [user_id.into()],
        ))
        .await?;
        Ok(())
    }
}
