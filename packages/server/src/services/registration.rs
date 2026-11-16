//! Whether self-registration is open: the fleet-wide switch in
//! [`platform_setting`], closed until an admin opens it.

use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Set};

use crate::entity::platform_setting;

/// Whether `POST /auth/register` creates accounts now. Closed when the
/// setting was never written.
pub async fn is_open<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    Ok(
        platform_setting::Entity::find_by_id(platform_setting::ROW_ID)
            .one(db)
            .await?
            .is_some_and(|row| row.registration_open),
    )
}

/// Open or close self-registration for every replica.
pub async fn set_open<C: ConnectionTrait>(db: &C, open: bool) -> Result<(), DbErr> {
    platform_setting::Entity::insert(platform_setting::ActiveModel {
        id: Set(platform_setting::ROW_ID),
        registration_open: Set(open),
    })
    .on_conflict(
        OnConflict::column(platform_setting::Column::Id)
            .update_column(platform_setting::Column::RegistrationOpen)
            .to_owned(),
    )
    .exec(db)
    .await?;
    Ok(())
}
