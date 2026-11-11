use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "idempotency_key")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,

    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i32,

    pub request_path: String,

    pub request_method: String,

    /// Hex SHA-256 of the request body. `None` on rows written before
    /// fingerprints existed.
    pub request_fingerprint: Option<String>,

    pub status: String,

    pub response_status: Option<i16>,

    #[sea_orm(column_type = "Text", nullable)]
    pub response_body: Option<String>,

    #[sea_orm(indexed)]
    pub created_at: DateTimeUtc,

    pub completed_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
