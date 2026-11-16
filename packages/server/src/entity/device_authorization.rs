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

    /// The sign-in session that approved the code; `None` while it is
    /// pending. Ending the session deletes the approval. The CLI's access
    /// token is minted from the session's user's current grants when the CLI
    /// collects it, never stored.
    #[sea_orm(nullable, indexed)]
    pub approved_by: Option<i64>,

    /// The approving session's user, written beside `approved_by` for
    /// servers of the previous release that read it while a release rolls
    /// out (see `m0013_device_approval_session`). No code of this build reads
    /// it.
    pub authorized_user_id: Option<i32>,

    /// The client address that requested the code, `None` when the server
    /// could not tell it. Pending codes are bounded per address.
    #[sea_orm(nullable, indexed)]
    pub client_addr: Option<String>,

    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub last_poll_at: Option<DateTimeUtc>,

    #[sea_orm(belongs_to, from = "approved_by", to = "id", on_delete = "Cascade")]
    pub approver: HasOne<super::auth_session::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}
