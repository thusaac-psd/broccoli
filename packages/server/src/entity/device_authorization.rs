use sea_orm::entity::prelude::*;

/// A pending device-flow login (RFC 8628), shared by every server replica.
///
/// The CLI creates the row on one replica, the browser authorizes it on
/// another, and the CLI polls a third, so this state cannot live in process
/// memory. Rows expire on the DB clock (`expires_at` against `now()`) and are
/// purged when new codes are requested.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "device_authorization")]
pub struct Model {
    /// SHA-256 of the device code. The code itself is a bearer secret for the
    /// poll endpoint and is never stored.
    #[sea_orm(primary_key, auto_increment = false)]
    pub device_code_hash: String,

    /// Normalized user code (upper-case letters, no separator), the value the
    /// browser submits.
    #[sea_orm(unique)]
    pub user_code: String,

    /// The user who approved the code. The access token is minted from this
    /// user's current grants when the CLI collects it, never stored.
    pub authorized_user_id: Option<i32>,

    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub last_poll_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
