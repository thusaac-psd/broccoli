use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "plugin")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub is_enabled: bool,
    /// Bumped by every lifecycle change (enable, disable, reload, upload) so
    /// each replica knows to re-apply this row. See `crate::plugin_lifecycle`.
    #[sea_orm(default_value = 0)]
    pub generation: i64,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
