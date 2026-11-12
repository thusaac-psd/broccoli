use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "user_role")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub user_id: i32,
    #[sea_orm(primary_key)]
    pub role: String,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,
    #[sea_orm(belongs_to, from = "role", to = "name")]
    pub role_ref: HasOne<super::role::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    /// Give `user_id` the role `role` unless they already hold it. Returns
    /// whether a row was inserted, so callers can skip side effects (session
    /// revocation) for a no-op.
    pub async fn grant<C: ConnectionTrait>(
        db: &C,
        user_id: i32,
        role: &str,
    ) -> Result<bool, DbErr> {
        let result = Self::insert(ActiveModel {
            user_id: sea_orm::Set(user_id),
            role: sea_orm::Set(role.to_owned()),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([Column::UserId, Column::Role])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await;
        match result {
            Ok(rows) => Ok(rows > 0),
            Err(DbErr::RecordNotInserted) => Ok(false),
            Err(e) => Err(e),
        }
    }
}
