//! Pending one-shot plugin timer callbacks. Rows are created by the
//! `timer_schedule` host function, claimed by the `dispatcher::plugin_timer`
//! loop, and deleted on successful delivery.
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "plugin_timer")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    pub plugin_id: String,
    pub key: String,

    pub fire_at: DateTimeUtc,

    #[sea_orm(column_type = "Text")]
    pub payload: String,

    #[sea_orm(nullable)]
    pub claimed_at: Option<DateTimeUtc>,

    #[sea_orm(default_value = 0)]
    pub attempts: i32,

    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
