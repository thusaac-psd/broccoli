use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "plugin")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub is_enabled: bool,
    /// Bumped by every lifecycle change (enable, disable, reload, upload,
    /// removing an upload) so each replica knows to re-apply this row. See
    /// `crate::plugin_lifecycle`.
    #[sea_orm(default_value = 0)]
    pub generation: i64,
    /// Manifest version and content hash of the plugin as published (by the
    /// replica that served the last enable or reload). A replica whose files
    /// fingerprint differently does not swap its running version for them.
    /// `None` until first published.
    #[sea_orm(nullable)]
    pub version: Option<String>,
    #[sea_orm(nullable)]
    pub content_hash: Option<String>,
    /// Content hash (hex) of the uploaded archive the fleet runs, in the
    /// shared blob store; `None` runs the copy bundled on each node. See
    /// `crate::plugin_distribution`.
    #[sea_orm(nullable)]
    pub archive_hash: Option<String>,
    /// What the plugin governs, as recorded by the last replica that loaded
    /// it (`crate::plugin_lifecycle::Governs`), so a replica that cannot load
    /// it still knows what to fail closed for. `None` until first loaded.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub governs: Option<Json>,
    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
