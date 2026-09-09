use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_agent_connection")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub id: String,
    pub org_id: i32,
    pub user_id: i32,
    #[sea_orm(column_type = "Text")]
    pub provider: String,
    pub generation: i64,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub account_label: Option<String>,
    pub checked_at: Option<DateTimeUtc>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_code: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub storage_handle: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
