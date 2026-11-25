use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "plugin_config")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub scope: String,
    #[sea_orm(primary_key)]
    pub ref_id: String,
    #[sea_orm(primary_key)]
    pub namespace: String,

    #[sea_orm(column_type = "JsonBinary")]
    pub config: Json,

    /// The refs of the secrets `config` holds, a list of
    /// `broccoli_types::manifest::SecretRef`.
    #[sea_orm(
        column_type = "JsonBinary",
        default_expr = "Expr::cust(\"'[]'::jsonb\")"
    )]
    pub secret_refs: Json,

    #[sea_orm(nullable)]
    pub enabled: Option<bool>,

    #[sea_orm(default_value = 0)]
    pub position: i32,

    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
