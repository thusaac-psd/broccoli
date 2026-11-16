use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "contest_problem")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub contest_id: i32,
    #[sea_orm(primary_key)]
    pub problem_id: i32,
    #[sea_orm(belongs_to, from = "contest_id", to = "id")]
    pub contest: HasOne<super::contest::Entity>,
    #[sea_orm(belongs_to, from = "problem_id", to = "id")]
    pub problem: HasOne<super::problem::Entity>,

    pub label: String,

    #[sea_orm(default_value = 0)]
    pub position: i32,
}

impl ActiveModelBehavior for ActiveModel {}
