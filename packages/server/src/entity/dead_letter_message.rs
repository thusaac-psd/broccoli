use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "dead_letter_message")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    #[sea_orm(unique)]
    pub message_id: String,

    #[sea_orm(column_name = "direction", indexed)]
    pub message_type: String,

    #[sea_orm(indexed)]
    pub submission_id: Option<i32>,

    #[sea_orm(column_type = "JsonBinary")]
    pub payload: serde_json::Value,

    #[sea_orm(column_type = "Text")]
    pub error_message: String,

    #[sea_orm(indexed)]
    pub error_code: String,

    /// Attempts recorded in `retry_history`: failed deliveries of an
    /// operation task, or each dead-lettering and operator retry of stuck
    /// work.
    pub retry_count: i32,

    #[sea_orm(column_type = "JsonBinary")]
    pub retry_history: serde_json::Value,

    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub first_failed_at: DateTimeUtc,

    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub created_at: DateTimeUtc,

    /// When the message was last dead-lettered (a recurrence reopens it),
    /// by the database clock.
    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub last_failed_at: DateTimeUtc,

    #[sea_orm(default_value = false, indexed)]
    pub resolved: bool,

    pub resolved_at: Option<DateTimeUtc>,

    pub resolved_by: Option<i32>,
}

impl ActiveModelBehavior for ActiveModel {}
