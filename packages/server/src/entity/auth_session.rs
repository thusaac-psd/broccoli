use sea_orm::entity::prelude::*;

/// One sign-in of a user: a login, or a device-flow approval. Every access
/// token names its session (the `sid` claim) and every refresh token belongs
/// to one, so ending a session (a logout) ends exactly that sign-in: its
/// refresh tokens go with the row, and its access tokens fail the freshness
/// check (`extractors::auth::fresh_user`) on their next mutating request.
/// The user's other sessions stay signed in.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "auth_session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    #[sea_orm(indexed)]
    pub user_id: i32,

    pub created_at: DateTimeUtc,

    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,

    #[sea_orm(has_many)]
    pub refresh_tokens: HasMany<super::refresh_token::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}
